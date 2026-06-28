//! Resume support: locate a resumable run and restore its scheduler state.
//!
//! A run is resumable when its `manifest.json` has `status == "running"` and a
//! `graph.json` checkpoint file exists. Only one such run may exist at a time;
//! if multiple are found the caller must refuse to proceed.
//!
//! Before handing the restored state back to the engine, `normalize_for_resume`
//! converts any in-flight nodes (`Running`, `Integrating`) back to `Pending` so
//! the scheduler re-dispatches them cleanly. This costs at most one re-run of the
//! last in-progress node but avoids any risk of the engine consuming a `Waiting`
//! state that has no matching pending effect.

use std::error::Error;
use std::path::{Path, PathBuf};

use crate::machines::scheduler::state::{NodeStatus, RunGraph, SchedulerState};
use crate::runtime::checkpoint::load_checkpoint;

/// Locate a single resumable run under `runs_root` and return its directory
/// alongside the normalized `SchedulerState` ready for re-entry.
///
/// Fails if:
/// - No directory with `manifest.json` { status: "running" } exists.
/// - More than one such directory exists.
/// - The matching run has no `graph.json` checkpoint.
/// - The checkpoint is corrupt or the state is already terminal.
pub fn find_resumable_run(runs_root: &Path) -> Result<(PathBuf, SchedulerState), Box<dyn Error>> {
    let running_dirs = collect_running_dirs(runs_root)?;

    let run_dir = match running_dirs.as_slice() {
        [] => {
            return Err("no resumable run found: no run directory has status=running".into());
        }
        [single] => single.clone(),
        multiple => {
            let ids: Vec<String> = multiple
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            return Err(format!(
                "multiple running runs found ({}); cannot resume unambiguously",
                ids.join(", ")
            )
            .into());
        }
    };

    let state = load_checkpoint(&run_dir)
        .map_err(|e| format!("run {} has no valid checkpoint: {e}", run_dir.display()))?;

    match &state {
        SchedulerState::Complete { .. } => {
            return Err(format!(
                "checkpoint for {} is Complete; run already finished — check manifest",
                run_dir.display()
            )
            .into());
        }
        SchedulerState::Failed { reason, .. } => {
            return Err(format!(
                "checkpoint for {} is Failed ({}); run already ended — check manifest",
                run_dir.display(),
                reason
            )
            .into());
        }
        _ => {}
    }

    let state = normalize_for_resume(state);
    Ok((run_dir, state))
}

/// Scan `runs_root` and return all run directories whose `manifest.json`
/// contains `"status": "running"`.
fn collect_running_dirs(runs_root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if !runs_root.is_dir() {
        return Err(format!("runs directory does not exist: {}", runs_root.display()).into());
    }

    let mut result = Vec::new();
    for entry in std::fs::read_dir(runs_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        if manifest_status_is_running(&manifest_path) {
            result.push(path);
        }
    }
    Ok(result)
}

fn manifest_status_is_running(manifest_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(manifest_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    v.get("status").and_then(|s| s.as_str()) == Some("running")
}

/// Convert a checkpointed state into one safe to hand to the engine loop.
///
/// - `Waiting { graph, .. }` becomes `Running { graph }` because the engine
///   boots with `Start` events; `Waiting + Start` would be a protocol violation.
/// - Any node in `Running` or `Integrating` status is reset to `Pending` so the
///   scheduler re-dispatches it. All other statuses are unchanged.
pub fn normalize_for_resume(state: SchedulerState) -> SchedulerState {
    match state {
        SchedulerState::Running { graph } => SchedulerState::Running {
            graph: reset_active_nodes(graph),
        },
        SchedulerState::Waiting { graph, .. } => SchedulerState::Running {
            graph: reset_active_nodes(graph),
        },
        other => other,
    }
}

fn reset_active_nodes(graph: RunGraph) -> RunGraph {
    let next_id = graph.next_id;
    RunGraph {
        nodes: graph
            .nodes
            .into_iter()
            .map(|mut n| {
                if matches!(n.status, NodeStatus::Running | NodeStatus::Integrating) {
                    n.status = NodeStatus::Pending;
                }
                n
            })
            .collect(),
        next_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::scheduler::state::{
        ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, SchedulerState,
    };
    use crate::runtime::checkpoint::save_checkpoint;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_runs_root(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("forge-resume-{label}-{}-{seq}", std::process::id()))
    }

    fn make_running_manifest(run_dir: &Path, status: &str) {
        let manifest = serde_json::json!({
            "run_id": run_dir.file_name().unwrap().to_str().unwrap(),
            "status": status,
        });
        std::fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn work_node(id: &str, status: NodeStatus) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            objective: format!("do {id}"),
            target_files: vec![],
            required_test_targets: vec![],
            dependencies: vec![],
            status,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
        }
    }

    fn sample_running_state() -> SchedulerState {
        SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    work_node("A", NodeStatus::Completed),
                    work_node("B", NodeStatus::Pending),
                ],
                next_id: 2,
            },
        }
    }

    // ── find_resumable_run ────────────────────────────────────────────────────

    #[test]
    fn resume_fails_when_no_runs_directory_exists() {
        let root = temp_runs_root("no-dir");
        let result = find_resumable_run(&root);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("does not exist"),
            "must say directory does not exist"
        );
    }

    #[test]
    fn resume_fails_when_no_running_manifest() {
        let root = temp_runs_root("no-running");
        std::fs::create_dir_all(&root).unwrap();
        let run = root.join("2026-01-01-00-00-01");
        std::fs::create_dir_all(&run).unwrap();
        make_running_manifest(&run, "succeeded");
        let result = find_resumable_run(&root);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("no resumable run"),
            "must say no resumable run"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resume_requires_running_manifest() {
        let root = temp_runs_root("requires-running");
        std::fs::create_dir_all(&root).unwrap();
        let run = root.join("2026-01-01-00-00-02");
        std::fs::create_dir_all(&run).unwrap();
        make_running_manifest(&run, "running");
        save_checkpoint(&run, &sample_running_state()).unwrap();
        let result = find_resumable_run(&root);
        assert!(result.is_ok(), "must succeed: {}", result.unwrap_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resume_fails_when_checkpoint_missing() {
        let root = temp_runs_root("no-checkpoint");
        std::fs::create_dir_all(&root).unwrap();
        let run = root.join("2026-01-01-00-00-03");
        std::fs::create_dir_all(&run).unwrap();
        make_running_manifest(&run, "running");
        // No graph.json written.
        let result = find_resumable_run(&root);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no valid checkpoint"),
            "must mention missing checkpoint; got: {msg}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resume_loads_graph_successfully() {
        let root = temp_runs_root("loads-graph");
        std::fs::create_dir_all(&root).unwrap();
        let run = root.join("2026-01-01-00-00-04");
        std::fs::create_dir_all(&run).unwrap();
        make_running_manifest(&run, "running");
        save_checkpoint(&run, &sample_running_state()).unwrap();
        let (loaded_dir, loaded_state) = find_resumable_run(&root).unwrap();
        assert_eq!(loaded_dir, run);
        let SchedulerState::Running { graph } = loaded_state else {
            panic!("expected Running state");
        };
        assert_eq!(graph.nodes.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resume_errors_on_multiple_running_manifests() {
        let root = temp_runs_root("multiple-running");
        std::fs::create_dir_all(&root).unwrap();
        for id in ["2026-01-01-00-00-05", "2026-01-01-00-00-06"] {
            let run = root.join(id);
            std::fs::create_dir_all(&run).unwrap();
            make_running_manifest(&run, "running");
            save_checkpoint(&run, &sample_running_state()).unwrap();
        }
        let result = find_resumable_run(&root);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("multiple running"),
            "must say multiple running"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── normalize_for_resume ─────────────────────────────────────────────────

    #[test]
    fn normalize_running_nodes_become_pending() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    work_node("A", NodeStatus::Completed),
                    work_node("B", NodeStatus::Running),
                ],
                next_id: 0,
            },
        };
        let SchedulerState::Running { graph } = normalize_for_resume(state) else {
            panic!("expected Running");
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
    }

    #[test]
    fn normalize_integrating_nodes_become_pending() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    work_node("A", NodeStatus::Completed),
                    work_node("B", NodeStatus::Integrating),
                ],
                next_id: 0,
            },
        };
        let SchedulerState::Running { graph } = normalize_for_resume(state) else {
            panic!("expected Running");
        };
        assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
    }

    #[test]
    fn normalize_waiting_becomes_running() {
        let graph = RunGraph {
            nodes: vec![
                work_node("A", NodeStatus::Completed),
                work_node("B", NodeStatus::Integrating),
            ],
            next_id: 0,
        };
        let state = SchedulerState::Waiting {
            graph,
            running: NodeId("B".to_string()),
        };
        let normalized = normalize_for_resume(state);
        assert!(
            matches!(normalized, SchedulerState::Running { .. }),
            "Waiting must become Running after normalization"
        );
        let SchedulerState::Running { graph } = normalized else {
            unreachable!()
        };
        assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
    }

    #[test]
    fn normalize_preserves_completed_and_failed_nodes() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    work_node("A", NodeStatus::Completed),
                    work_node("B", NodeStatus::Failed),
                    work_node("C", NodeStatus::Cancelled),
                    work_node("D", NodeStatus::Pending),
                ],
                next_id: 0,
            },
        };
        let SchedulerState::Running { graph } = normalize_for_resume(state) else {
            panic!("expected Running");
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert_eq!(graph.nodes[1].status, NodeStatus::Failed);
        assert_eq!(graph.nodes[2].status, NodeStatus::Cancelled);
        assert_eq!(graph.nodes[3].status, NodeStatus::Pending);
    }
}
