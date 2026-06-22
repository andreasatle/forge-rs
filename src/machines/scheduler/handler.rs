//! Effect handler for the scheduler machine.
//!
//! [`SchedulerHandler`] implements [`Machine`] by delegating pure transition
//! logic to [`SchedulerMachine`] and forwarding [`SchedulerEffect::RunNode`]
//! effects to a [`NodeRunner`].
//!
//! The scheduler itself does not know how node outcomes are produced. All fake
//! or real execution responsibility belongs here, behind the [`NodeRunner`]
//! boundary.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::artifacts::{Artifact, ArtifactView, create_workspace, integrate};
use crate::engine::{Machine, Transition};
use crate::machines::scheduler::effect::SchedulerEffect;
use crate::machines::scheduler::event::{
    IntegrationOutcome, IntegrationOutput, NodeOutcome, SchedulerEvent,
};
use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
use crate::machines::scheduler::state::SchedulerState;
use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunner};

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Drives the scheduler machine using a [`NodeRunner`] to execute nodes.
///
/// All pure transition logic is delegated to [`SchedulerMachine`]. This type
/// owns only effect execution: converting a `RunNode` effect into a runner
/// call and translating the result back into a `NodeReturned` event.
///
/// When an [`Artifact`] is supplied via [`with_artifact`](Self::with_artifact),
/// the handler:
///
/// - passes an [`ArtifactView`] into every [`NodeRunRequest`] so runners can
///   read the current committed state, and
/// - applies any [`ArtifactUpdate`](crate::artifacts::ArtifactUpdate) returned
///   by a work node, integrating it into the artifact before returning
///   `NodeReturned`.
pub struct SchedulerHandler<R> {
    runner: R,
    artifact: RefCell<Option<Artifact>>,
}

impl<R: NodeRunner> SchedulerHandler<R> {
    /// Create a new handler backed by the given [`NodeRunner`], with no artifact.
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            artifact: RefCell::new(None),
        }
    }

    /// Create a handler that owns an [`Artifact`] and keeps it current across
    /// work node executions.
    pub fn with_artifact(runner: R, artifact: Artifact) -> Self {
        Self {
            runner,
            artifact: RefCell::new(Some(artifact)),
        }
    }
}

impl<R: NodeRunner> Machine for SchedulerHandler<R> {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerOutput;

    fn start_event(&self) -> SchedulerEvent {
        SchedulerMachine.start_event()
    }

    fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        SchedulerMachine.transition(state, event)
    }

    fn handle_effect(&self, effect: SchedulerEffect) -> SchedulerEvent {
        match effect {
            SchedulerEffect::RunNode {
                node_id,
                kind,
                objective,
                model_tier,
                attempt,
            } => {
                // Snapshot the current artifact before running the node.
                // The clone is cheap (three fields) and avoids holding a borrow
                // across the runner call and the integration that follows.
                let artifact_snapshot = self.artifact.borrow().clone();

                let artifact_view = artifact_snapshot.as_ref().map(|a| ArtifactView {
                    repo_path: a.repo_path.clone(),
                    commit_sha: a.commit_sha.clone(),
                });

                let request = NodeRunRequest {
                    kind,
                    objective,
                    model_tier,
                    attempt,
                    artifact_view,
                };
                let result = self.runner.run_node(request);

                // If the work node produced file changes and we have an artifact,
                // apply the update and push a new commit.
                if let NodeRunResult::WorkAccepted(ref work_result) = result
                    && let (Some(update), Some(artifact)) =
                        (&work_result.artifact_update, &artifact_snapshot)
                {
                    let seq = WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let workspace_path = std::env::temp_dir()
                        .join(format!("forge-workspace-{}-{seq}", std::process::id()));
                    let mut workspace = create_workspace(artifact, workspace_path);
                    if update.apply(&mut workspace).is_ok() {
                        let new_artifact = integrate(artifact, &workspace);
                        *self.artifact.borrow_mut() = Some(new_artifact);
                    }
                }

                SchedulerEvent::NodeReturned {
                    node_id,
                    outcome: NodeOutcome::from(result),
                }
            }

            SchedulerEffect::IntegrateWork { node_id, work } => {
                SchedulerEvent::IntegrationReturned {
                    node_id,
                    outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                        summary: work.summary,
                    }),
                }
            }

            SchedulerEffect::ReturnComplete { .. } | SchedulerEffect::ReturnFailed { .. } => {
                unreachable!("return effects are never dispatched to the effect handler")
            }
        }
    }

    fn output(&self, state: &SchedulerState) -> Option<SchedulerOutput> {
        SchedulerMachine.output(state)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifacts::{ArtifactUpdate, FileChange};
    use crate::engine::{Machine, run_machine};
    use crate::machines::scheduler::effect::SchedulerEffect;
    use crate::machines::scheduler::event::WorkOutput;
    use crate::machines::scheduler::event::{NodeOutcome, SchedulerEvent};
    use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
    use crate::machines::scheduler::state::{
        ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest,
        SchedulerState,
    };
    use crate::node_runner::runner::NodeRunner;
    use crate::node_runner::types::{NodeRunRequest, NodeRunResult};
    use crate::node_runner::{NodeRunWorkResult, StaticNodeRunner};

    // ── test helpers ──────────────────────────────────────────────────────────

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-handler-test-{label}-{}-{seq}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("failed to create temporary test directory");
            Self(path)
        }

        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("failed to execute git in test");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("failed to execute git in test");
        assert!(out.status.success(), "git {} failed", args.join(" "));
        String::from_utf8(out.stdout)
            .expect("git output was not UTF-8")
            .trim()
            .to_owned()
    }

    fn git_clone_bare(source: &Path, destination: &Path) {
        let status = Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(source)
            .arg(destination)
            .status()
            .expect("failed to create bare test repository");
        assert!(status.success(), "git clone --bare failed");
    }

    /// Build a bare-repository-backed artifact with a single initial commit.
    fn fixture(label: &str) -> (TempDirectory, Artifact) {
        let temp = TempDirectory::new(label);
        let seed_path = temp.join("seed");
        fs::create_dir(&seed_path).expect("failed to create seed directory");
        git(&seed_path, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed_path, &["config", "user.name", "Handler Test"]);
        git(
            &seed_path,
            &["config", "user.email", "handler-test@example.invalid"],
        );
        fs::write(seed_path.join("artifact.txt"), "version one\n")
            .expect("failed to write fixture file");
        git(&seed_path, &["add", "artifact.txt"]);
        git(&seed_path, &["commit", "--quiet", "-m", "Initial"]);
        let repo_path = temp.join("artifact.git");
        git_clone_bare(&seed_path, &repo_path);
        let commit_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
        (
            temp,
            Artifact {
                repo_path,
                branch: "main".to_owned(),
                commit_sha,
            },
        )
    }

    fn handler() -> SchedulerHandler<StaticNodeRunner> {
        SchedulerHandler::new(StaticNodeRunner)
    }

    fn work_node(id: &str, objective: &str) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            objective: objective.to_string(),
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
        }
    }

    fn work_node_with_deps(id: &str, objective: &str, deps: &[&str]) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            objective: objective.to_string(),
            dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
        }
    }

    // ── existing tests (unchanged) ────────────────────────────────────────────

    #[test]
    fn run_node_effect_uses_node_runner() {
        let h = handler();
        let effect = SchedulerEffect::RunNode {
            node_id: NodeId("n1".to_string()),
            kind: NodeKind::Work,
            objective: "write some code".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        };
        let event = h.handle_effect(effect);
        let SchedulerEvent::NodeReturned { outcome, .. } = event else {
            panic!("expected NodeReturned, got {event:#?}");
        };
        assert!(matches!(outcome, NodeOutcome::WorkAccepted(_)));
    }

    #[test]
    fn plan_node_flows_through_runner() {
        let state = SchedulerMachine::initial_state(RunRequest {
            objective: "plan the work".to_string(),
        });
        let output = run_machine(handler(), state);
        assert!(
            matches!(output, SchedulerOutput::Complete { .. }),
            "expected Complete, got {output:#?}"
        );
    }

    #[test]
    fn work_node_flows_through_runner() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "build artifacts")],
                next_id: 0,
            },
        };
        let output = run_machine(handler(), state);
        assert!(
            matches!(output, SchedulerOutput::Complete { .. }),
            "expected Complete, got {output:#?}"
        );
    }

    #[test]
    fn failed_node_flows_through_runner() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("F", "fail this step")],
                next_id: 0,
            },
        };
        let output = run_machine(handler(), state);
        assert!(
            matches!(output, SchedulerOutput::Failed { .. }),
            "expected Failed, got {output:#?}"
        );
    }

    // ── artifact integration tests ────────────────────────────────────────────

    /// Records the artifact view received on each `run_node` call.
    struct ViewCapturingRunner {
        views: Rc<RefCell<Vec<Option<ArtifactView>>>>,
    }

    impl NodeRunner for ViewCapturingRunner {
        fn run_node(&self, request: NodeRunRequest) -> NodeRunResult {
            self.views.borrow_mut().push(request.artifact_view);
            NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: "captured".to_string(),
                },
                artifact_update: None,
            })
        }
    }

    /// Returns a work node result that writes a single file.
    struct FileWritingRunner {
        path: String,
        content: String,
    }

    impl NodeRunner for FileWritingRunner {
        fn run_node(&self, _request: NodeRunRequest) -> NodeRunResult {
            NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: format!("wrote {}", self.path),
                },
                artifact_update: Some(ArtifactUpdate {
                    changes: vec![FileChange::Write {
                        path: self.path.clone(),
                        content: self.content.clone(),
                    }],
                }),
            })
        }
    }

    /// On the first call writes a file; on the second call records the received view.
    struct TwoStepRunner {
        call_count: RefCell<u32>,
        second_view: Rc<RefCell<Option<ArtifactView>>>,
    }

    impl NodeRunner for TwoStepRunner {
        fn run_node(&self, request: NodeRunRequest) -> NodeRunResult {
            let count = {
                let mut c = self.call_count.borrow_mut();
                *c += 1;
                *c
            };
            match count {
                1 => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                    work: WorkOutput {
                        summary: "step one".to_string(),
                    },
                    artifact_update: Some(ArtifactUpdate {
                        changes: vec![FileChange::Write {
                            path: "step1.txt".to_string(),
                            content: "written by node one\n".to_string(),
                        }],
                    }),
                }),
                2 => {
                    *self.second_view.borrow_mut() = request.artifact_view;
                    NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "step two".to_string(),
                        },
                        artifact_update: None,
                    })
                }
                n => panic!("unexpected call count: {n}"),
            }
        }
    }

    #[test]
    fn scheduler_handler_passes_artifact_view_to_node_runner() {
        let (_temp, artifact) = fixture("passes-view");
        let expected_sha = artifact.commit_sha.clone();

        let views = Rc::new(RefCell::new(Vec::new()));
        let runner = ViewCapturingRunner {
            views: views.clone(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("n1".to_string()),
            kind: NodeKind::Work,
            objective: "do something".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let captured = views.borrow();
        assert_eq!(captured.len(), 1, "runner must be called exactly once");
        let view = captured[0]
            .as_ref()
            .expect("runner must receive Some(ArtifactView)");
        assert_eq!(
            view.commit_sha, expected_sha,
            "view must point at the artifact's current commit"
        );
    }

    #[test]
    fn work_node_artifact_update_creates_new_commit() {
        let (_temp, artifact) = fixture("creates-commit");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello from work node\n".to_string(),
        };
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "write a file")],
                next_id: 0,
            },
        };
        run_machine(SchedulerHandler::with_artifact(runner, artifact), state);

        let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_ne!(
            new_sha, original_sha,
            "commit SHA must advance after an artifact update"
        );
    }

    #[test]
    fn second_work_node_sees_first_work_node_changes() {
        let (_temp, artifact) = fixture("second-sees-first");

        let second_view = Rc::new(RefCell::new(None));
        let runner = TwoStepRunner {
            call_count: RefCell::new(0),
            second_view: second_view.clone(),
        };

        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    work_node_with_deps("A", "write the file", &[]),
                    work_node_with_deps("B", "read the file", &["A"]),
                ],
                next_id: 0,
            },
        };
        run_machine(SchedulerHandler::with_artifact(runner, artifact), state);

        let view = second_view.borrow();
        let view = view
            .as_ref()
            .expect("node B must receive Some(ArtifactView)");
        let content = view
            .read_file("step1.txt")
            .expect("step1.txt must be visible to node B via its ArtifactView");
        assert_eq!(
            content, "written by node one\n",
            "node B must see the file written by node A"
        );
    }

    #[test]
    fn work_node_without_update_preserves_commit() {
        let (_temp, artifact) = fixture("no-update-preserves");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "do some work")],
                next_id: 0,
            },
        };
        run_machine(
            SchedulerHandler::with_artifact(StaticNodeRunner, artifact),
            state,
        );

        let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_after, original_sha,
            "commit SHA must not change when the runner produces no artifact update"
        );
    }
}
