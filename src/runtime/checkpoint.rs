//! Checkpoint serialization for the scheduler graph.
//!
//! `save_checkpoint` writes the current `SchedulerState` to `graph.json` inside
//! the run directory. `load_checkpoint` reads it back and validates it is
//! loadable. Neither function validates scheduler-level invariants; that is the
//! responsibility of the transition layer.
//!
//! Checkpoints are written after scheduler progress events in
//! `SchedulerHandler`. They are never written for terminal states (`Complete`,
//! `Failed`), because the manifest is finalized before the process exits and a
//! terminal state does not need to be resumed.

use std::error::Error;
use std::path::Path;

use crate::machines::scheduler::SchedulerState;

const CHECKPOINT_FILE: &str = "graph.json";

/// Write `state` as pretty-printed JSON to `<run_dir>/graph.json`.
///
/// Overwrites any existing checkpoint. Returns an error if serialization or
/// file I/O fails; callers should treat the error as non-fatal and log a
/// warning rather than aborting the run.
pub fn save_checkpoint(run_dir: &Path, state: &SchedulerState) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(run_dir.join(CHECKPOINT_FILE), json)?;
    Ok(())
}

/// Load and deserialize `<run_dir>/graph.json`.
///
/// Returns an error if the file is missing or contains malformed JSON. The
/// caller is responsible for deciding whether the loaded state is valid for
/// resume (e.g. refusing to resume from `Complete` or `Failed`).
pub fn load_checkpoint(run_dir: &Path) -> Result<SchedulerState, Box<dyn Error>> {
    let path = run_dir.join(CHECKPOINT_FILE);
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("checkpoint not found at {}: {e}", path.display()))?;
    let state: SchedulerState = serde_json::from_str(&content)
        .map_err(|e| format!("corrupt checkpoint at {}: {e}", path.display()))?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::scheduler::{
        ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunConfig, RunGraph,
        SchedulerState,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-checkpoint-{label}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn work_node(id: &str) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            team: String::new(),
            task_id: None,
            worker_role: None,
            objective: format!("objective for {id}"),
            target_files: vec![],
            required_validation_targets: vec![],
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
            retry_feedback: None,
        }
    }

    fn sample_graph() -> RunGraph {
        let mut a = work_node("A");
        a.status = NodeStatus::Completed;
        a.summary = Some("done A".to_string());
        let b = work_node("B");
        RunGraph { nodes: vec![a, b] }
    }

    #[test]
    fn scheduler_state_round_trip() {
        let waiting_graph = {
            let mut graph = sample_graph();
            graph.nodes[1].status = NodeStatus::Integrating;
            graph
        };
        let recovery_graph = {
            let mut original = work_node("W");
            original.status = NodeStatus::Failed;
            let mut retry = work_node("W-retry-1");
            retry.origin = NodeOrigin::Retry {
                source: NodeId("W".to_string()),
            };
            retry.attempt = 1;
            RunGraph {
                nodes: vec![original, retry],
            }
        };

        let cases = [
            (
                "active",
                SchedulerState::Active {
                    graph: sample_graph(),
                    run_config: RunConfig::default(),
                },
            ),
            (
                "waiting",
                SchedulerState::Waiting {
                    graph: waiting_graph,
                    run_config: RunConfig::default(),
                },
            ),
            (
                "active-with-recovery-origin",
                SchedulerState::Active {
                    graph: recovery_graph,
                    run_config: RunConfig::default(),
                },
            ),
        ];

        for (label, state) in cases {
            let dir = temp_dir(&format!("state-round-trip-{label}"));
            save_checkpoint(&dir, &state).unwrap();
            let loaded = load_checkpoint(&dir).unwrap();
            assert_eq!(state, loaded, "case: {label}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn active_state_uses_stable_checkpoint_tag() {
        let dir = temp_dir("active-checkpoint-tag");
        let state = SchedulerState::Active {
            graph: sample_graph(),
            run_config: RunConfig::default(),
        };
        save_checkpoint(&dir, &state).unwrap();
        let raw = std::fs::read_to_string(dir.join("graph.json")).unwrap();
        assert!(
            raw.contains("\"Running\""),
            "checkpoint tag must remain stable; got: {raw}"
        );
        let loaded = load_checkpoint(&dir).unwrap();
        assert_eq!(state, loaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_checkpoint_fails_when_file_missing() {
        let dir = temp_dir("missing-file");
        let result = load_checkpoint(&dir);
        assert!(result.is_err(), "must fail when graph.json is absent");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("checkpoint not found"),
            "error must mention checkpoint not found; got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_checkpoint_fails_cleanly() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join("graph.json"), b"not valid json").unwrap();
        let result = load_checkpoint(&dir);
        assert!(result.is_err(), "must fail on corrupt JSON");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("corrupt checkpoint"),
            "error must mention corrupt checkpoint; got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_counts_reflects_completed_nodes() {
        let graph = sample_graph();
        let (total, completed) = graph.node_counts();
        assert_eq!(total, 2);
        assert_eq!(completed, 1);
    }
}
