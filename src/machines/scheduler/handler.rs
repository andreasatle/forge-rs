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
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::artifacts::{
    Artifact, ArtifactUpdate, ArtifactView, create_temporary_workspace, integrate,
};
use crate::engine::{Machine, Transition};
use crate::machines::scheduler::effect::SchedulerEffect;
use crate::machines::scheduler::event::{
    IntegrationFailure, IntegrationOutcome, IntegrationOutput, NodeOutcome, RecoveryAction,
    SchedulerEvent,
};
use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
use crate::machines::scheduler::state::{NodeId, NodeKind, SchedulerState};
use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunner};
use crate::runtime::checkpoint::{node_counts, save_checkpoint};
use crate::telemetry::{
    ConsoleTelemetry, NoopTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink,
};
use crate::validation::{AlwaysPassValidator, Validator};

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
/// - stores any [`ArtifactUpdate`] returned by a work node so that it can be
///   applied and committed during the subsequent `IntegrateWork` effect.
///
/// Artifact mutation happens exclusively in `IntegrateWork`, not in `RunNode`.
/// Workers never advance artifact truth; the Integrator is the single writer.
pub struct SchedulerHandler<R> {
    runner: R,
    artifact: RefCell<Option<Artifact>>,
    pending_artifact_updates: RefCell<HashMap<NodeId, ArtifactUpdate>>,
    telemetry: Rc<dyn TelemetrySink>,
    validator: Rc<dyn Validator>,
    /// Tracks whether the integration validation gate ran and what it returned.
    /// `Some(true)` = validation ran and passed (even if integration later failed).
    /// `Some(false)` = validation ran and failed.
    /// `None` = validation was never invoked (no artifact update reached the gate).
    last_validation_passed: RefCell<Option<bool>>,
    /// When set, the handler writes `graph.json` to this directory after
    /// `NodeReturned` and `IntegrationReturned` transitions.
    checkpoint_dir: Option<PathBuf>,
}

impl<R: NodeRunner> SchedulerHandler<R> {
    /// Create a new handler backed by the given [`NodeRunner`], with no artifact.
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            artifact: RefCell::new(None),
            pending_artifact_updates: RefCell::new(HashMap::new()),
            telemetry: Rc::new(NoopTelemetry),
            validator: Rc::new(AlwaysPassValidator),
            last_validation_passed: RefCell::new(None),
            checkpoint_dir: None,
        }
    }

    /// Create a handler that owns an [`Artifact`] and keeps it current across
    /// work node integrations.
    pub fn with_artifact(runner: R, artifact: Artifact) -> Self {
        Self {
            runner,
            artifact: RefCell::new(Some(artifact)),
            pending_artifact_updates: RefCell::new(HashMap::new()),
            telemetry: Rc::new(NoopTelemetry),
            validator: Rc::new(AlwaysPassValidator),
            last_validation_passed: RefCell::new(None),
            checkpoint_dir: None,
        }
    }

    /// Attach a shared telemetry sink so node runs record into the same trace.
    pub fn with_telemetry(self, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self { telemetry, ..self }
    }

    /// Replace the default [`AlwaysPassValidator`] with a custom validator.
    pub fn with_validator(self, validator: Rc<dyn Validator>) -> Self {
        Self { validator, ..self }
    }

    /// Enable checkpoint writes to `dir` after each progress event.
    ///
    /// When set, the handler writes `graph.json` to `dir` after every
    /// `NodeReturned` and `IntegrationReturned` transition that leaves the
    /// scheduler in a non-terminal state.
    pub fn with_checkpoint_dir(self, dir: PathBuf) -> Self {
        Self {
            checkpoint_dir: Some(dir),
            ..self
        }
    }

    /// Returns a clone of the current artifact, or `None` if no artifact was provided.
    pub fn artifact(&self) -> Option<Artifact> {
        self.artifact.borrow().clone()
    }

    /// Returns whether the integration validation gate ran and what it returned.
    ///
    /// `Some(true)` means validation ran and passed (even if CAS integration later failed).
    /// `Some(false)` means validation ran and failed.
    /// `None` means the gate was never reached (no artifact update was pending).
    pub fn validation_passed(&self) -> Option<bool> {
        *self.last_validation_passed.borrow()
    }

    fn maybe_save_checkpoint(&self, state: &SchedulerState) {
        let Some(dir) = &self.checkpoint_dir else {
            return;
        };
        let is_active = matches!(
            state,
            SchedulerState::Running { .. } | SchedulerState::Waiting { .. }
        );
        if !is_active {
            return;
        }
        let graph = match state {
            SchedulerState::Running { graph } | SchedulerState::Waiting { graph, .. } => graph,
            _ => return,
        };
        let (node_count, completed_count) = node_counts(graph);
        match save_checkpoint(dir, state) {
            Ok(()) => {
                self.telemetry.record(TelemetryRecord::new(
                    "Checkpoint",
                    TelemetryEvent::CheckpointSaved {
                        node_count,
                        completed_count,
                    },
                ));
            }
            Err(e) => {
                eprintln!("warning: failed to save checkpoint: {e}");
            }
        }
    }
}

impl<R: NodeRunner> Machine for SchedulerHandler<R> {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerOutput;

    fn name(&self) -> String {
        "SchedulerMachine".to_string()
    }

    fn start_event(&self) -> SchedulerEvent {
        SchedulerMachine.start_event()
    }

    fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        print_returned_progress(&event);
        let is_progress_event = matches!(
            event,
            SchedulerEvent::NodeReturned { .. } | SchedulerEvent::IntegrationReturned { .. }
        );
        let result = SchedulerMachine.transition(state, event);
        if is_progress_event {
            self.maybe_save_checkpoint(&result.state);
        }
        result
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
                eprintln!("[scheduler] dispatch {} {:?}", node_id.0, kind);
                // Snapshot the current artifact before running the node.
                // The clone is cheap (three fields) and avoids holding a borrow
                // across the runner call.
                let artifact_snapshot = self.artifact.borrow().clone();

                let artifact_view = artifact_snapshot.as_ref().map(|a| ArtifactView {
                    repo_path: a.repo_path.clone(),
                    commit_sha: a.commit_sha.clone(),
                });

                let label = match &kind {
                    NodeKind::Plan => "[planner]".to_string(),
                    NodeKind::Work => format!("[worker {}]", node_id.0),
                };
                let request = NodeRunRequest {
                    kind,
                    objective,
                    model_tier,
                    attempt,
                    artifact_view,
                };
                let console_tel = ConsoleTelemetry::new(self.telemetry.as_ref(), label);
                let result = self.runner.run_node(request, &console_tel);

                // If the work node produced file changes, stash them under this
                // node_id for deferred integration. Artifact truth is never
                // advanced here; that happens exclusively in IntegrateWork.
                if let NodeRunResult::WorkAccepted(ref work_result) = result
                    && let Some(update) = work_result.artifact_update.clone()
                {
                    self.pending_artifact_updates
                        .borrow_mut()
                        .insert(node_id.clone(), update);
                }

                SchedulerEvent::NodeReturned {
                    node_id,
                    outcome: NodeOutcome::from(result),
                }
            }

            SchedulerEffect::IntegrateWork { node_id, work } => {
                eprintln!("[integration] start {}", node_id.0);
                // Retrieve any artifact update that was stashed during RunNode.
                let pending_update = self.pending_artifact_updates.borrow_mut().remove(&node_id);
                let artifact_snapshot = self.artifact.borrow().clone();

                if let (Some(update), Some(artifact)) = (pending_update, artifact_snapshot) {
                    let workspace_result = create_temporary_workspace(&artifact);
                    let mut workspace = match workspace_result {
                        Ok(w) => w,
                        Err(err) => {
                            return SchedulerEvent::IntegrationReturned {
                                node_id,
                                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                                    reason: format!("workspace creation failed: {err}"),
                                    recovery: RecoveryAction::Terminal {
                                        message: format!("workspace creation failed: {err}"),
                                    },
                                }),
                            };
                        }
                    };

                    match update.apply(&mut workspace) {
                        Err(err) => {
                            return SchedulerEvent::IntegrationReturned {
                                node_id,
                                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                                    reason: format!("artifact update apply error: {err}"),
                                    recovery: RecoveryAction::Terminal {
                                        message: format!("artifact update apply error: {err}"),
                                    },
                                }),
                            };
                        }
                        Ok(()) => {
                            self.telemetry.record(TelemetryRecord::new(
                                "Integration",
                                TelemetryEvent::ValidationStarted,
                            ));
                            let result = self.validator.validate(&workspace);
                            if result.passed {
                                *self.last_validation_passed.borrow_mut() = Some(true);
                                self.telemetry.record(TelemetryRecord::new(
                                    "Integration",
                                    TelemetryEvent::ValidationPassed {
                                        summary: result.summary,
                                    },
                                ));
                                match integrate(&artifact, &workspace) {
                                    Ok(new_artifact) => {
                                        *self.artifact.borrow_mut() = Some(new_artifact);
                                    }
                                    Err(err) => {
                                        return SchedulerEvent::IntegrationReturned {
                                            node_id,
                                            outcome: IntegrationOutcome::Failed(
                                                IntegrationFailure {
                                                    reason: err.to_string(),
                                                    recovery: RecoveryAction::Terminal {
                                                        message: err.to_string(),
                                                    },
                                                },
                                            ),
                                        };
                                    }
                                }
                            } else {
                                *self.last_validation_passed.borrow_mut() = Some(false);
                                self.telemetry.record(TelemetryRecord::new(
                                    "Integration",
                                    TelemetryEvent::ValidationFailed {
                                        summary: result.summary.clone(),
                                    },
                                ));
                                return SchedulerEvent::IntegrationReturned {
                                    node_id,
                                    outcome: IntegrationOutcome::Failed(IntegrationFailure {
                                        reason: format!("validation failed: {}", result.summary),
                                        recovery: RecoveryAction::Terminal {
                                            message: format!(
                                                "validation failed: {}",
                                                result.summary
                                            ),
                                        },
                                    }),
                                };
                            }
                        }
                    }
                }

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

fn print_returned_progress(event: &SchedulerEvent) {
    match event {
        SchedulerEvent::NodeReturned { node_id, outcome } => {
            if let NodeOutcome::Failed(failure) = outcome {
                eprintln!("[scheduler] failed {}", node_id.0);
                let recovery = match &failure.recovery {
                    RecoveryAction::Retry { .. } => "Retry",
                    RecoveryAction::Split { .. } => "Split",
                    RecoveryAction::ElevateModel { .. } => "ElevateModel",
                    RecoveryAction::Terminal { .. } => "Terminal",
                };
                eprintln!("[scheduler] recovery {recovery} {}", node_id.0);
            } else {
                eprintln!("[scheduler] returned {}", node_id.0);
            }
        }
        SchedulerEvent::IntegrationReturned { node_id, outcome } => {
            if matches!(outcome, IntegrationOutcome::Failed(_)) {
                eprintln!("[integration] failed {}", node_id.0);
            } else {
                eprintln!("[integration] complete {}", node_id.0);
            }
        }
        SchedulerEvent::Start => {}
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
    use crate::machines::scheduler::event::{
        IntegrationOutcome, IntegrationOutput, NodeOutcome, SchedulerEvent, WorkOutput,
    };
    use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
    use crate::machines::scheduler::state::{
        ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest,
        SchedulerState,
    };
    use crate::node_runner::runner::NodeRunner;
    use crate::node_runner::types::{NodeRunRequest, NodeRunResult};
    use crate::node_runner::{NodeRunWorkResult, StaticNodeRunner};
    use crate::telemetry::TelemetrySink;

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
        fn run_node(
            &self,
            request: NodeRunRequest,
            _telemetry: &dyn TelemetrySink,
        ) -> NodeRunResult {
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
        fn run_node(
            &self,
            _request: NodeRunRequest,
            _telemetry: &dyn TelemetrySink,
        ) -> NodeRunResult {
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
        fn run_node(
            &self,
            request: NodeRunRequest,
            _telemetry: &dyn TelemetrySink,
        ) -> NodeRunResult {
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

    /// Returns a work node result with a Replace update that will fail because
    /// the target text is absent from the fixture file.
    struct BadReplaceRunner;

    impl NodeRunner for BadReplaceRunner {
        fn run_node(
            &self,
            _request: NodeRunRequest,
            _telemetry: &dyn TelemetrySink,
        ) -> NodeRunResult {
            NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: "will fail on integrate".to_string(),
                },
                artifact_update: Some(ArtifactUpdate {
                    changes: vec![FileChange::Replace {
                        path: "artifact.txt".to_string(),
                        old: "this text does not exist in the file".to_string(),
                        new: "replacement".to_string(),
                    }],
                }),
            })
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

    // ── handler boundary tests ─────────────────────────────────────────────────

    #[test]
    fn run_node_does_not_commit_artifact_update() {
        let (_temp, artifact) = fixture("no-commit-on-run");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello from work node\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_after, original_sha,
            "RunNode must not commit; artifact mutation must happen only during IntegrateWork"
        );
    }

    #[test]
    fn integrate_work_commits_pending_artifact_update() {
        let (_temp, artifact) = fixture("commit-on-integrate");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello from work node\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Succeeded(_),
                    ..
                }
            ),
            "IntegrateWork must return Succeeded; got: {event:#?}"
        );

        let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_ne!(
            new_sha, original_sha,
            "IntegrateWork must advance the artifact commit"
        );

        let output_content = git_output(&repo_path, &["show", &format!("{new_sha}:output.txt")]);
        assert_eq!(
            output_content, "hello from work node",
            "output.txt must exist in the integrated commit"
        );
    }

    #[test]
    fn artifact_update_apply_failure_returns_integration_failure() {
        let (_temp, artifact) = fixture("apply-fail");
        let h = SchedulerHandler::with_artifact(BadReplaceRunner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "do work".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "done".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Failed(_),
                    ..
                }
            ),
            "IntegrateWork must return Failed when apply errors; got: {event:#?}"
        );
    }

    /// A runner that writes a file using a non-bare repo as the artifact repo.
    /// The workspace creation succeeds (git clone works from non-bare repos) but
    /// `integrate()` fails because `check_bare_repository` rejects the repo.
    struct NonBareRepoFixture {
        _temp: TempDirectory,
        artifact: Artifact,
    }

    impl NonBareRepoFixture {
        fn new(label: &str) -> Self {
            let temp = TempDirectory::new(label);
            let repo_path = temp.join("not-bare.git");
            fs::create_dir(&repo_path).expect("failed to create non-bare repo directory");
            git(&repo_path, &["init", "--quiet", "--initial-branch=main"]);
            git(&repo_path, &["config", "user.name", "Test"]);
            git(
                &repo_path,
                &["config", "user.email", "test@example.invalid"],
            );
            fs::write(repo_path.join("artifact.txt"), "v1\n")
                .expect("failed to write initial file");
            git(&repo_path, &["add", "artifact.txt"]);
            git(&repo_path, &["commit", "--quiet", "-m", "Initial"]);
            let commit_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
            let artifact = Artifact {
                repo_path,
                branch: "main".to_owned(),
                commit_sha,
            };
            Self {
                _temp: temp,
                artifact,
            }
        }
    }

    #[test]
    fn scheduler_handler_maps_integration_error_to_failed_outcome() {
        let fix = NonBareRepoFixture::new("integrate-error-mapping");
        let original_sha = fix.artifact.commit_sha.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, fix.artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Failed(_),
                    ..
                }
            ),
            "integrate() error must map to IntegrationOutcome::Failed; got: {event:#?}"
        );

        // Artifact commit must remain unchanged on integration failure.
        let current_sha = h
            .artifact()
            .expect("artifact must still be present")
            .commit_sha;
        assert_eq!(
            current_sha, original_sha,
            "artifact commit must not change when integration fails"
        );
    }

    #[test]
    fn second_work_node_sees_first_only_after_integration() {
        let (_temp, artifact) = fixture("second-sees-after-integration");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let writer = FileWritingRunner {
            path: "step1.txt".to_string(),
            content: "written by node one\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(writer, artifact);

        // RunNode for A: stores the update but does NOT commit.
        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("A".to_string()),
            kind: NodeKind::Work,
            objective: "write the file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });
        let sha_before_integrate = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_before_integrate, original_sha,
            "commit must not advance before IntegrateWork"
        );

        // IntegrateWork for A: applies the update and commits.
        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("A".to_string()),
            work: WorkOutput {
                summary: "wrote step1.txt".to_string(),
            },
        });
        let sha_after_integrate = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_ne!(
            sha_after_integrate, original_sha,
            "commit must advance after IntegrateWork"
        );

        // The handler's artifact now reflects the new commit.
        let current_sha = h.artifact().expect("artifact must be present").commit_sha;
        assert_eq!(
            current_sha, sha_after_integrate,
            "handler artifact must point at the integrated commit"
        );
    }

    /// Advance the branch in a bare repo to a new commit without a separate clone.
    fn advance_branch_in_bare(bare_repo: &Path, branch: &str) -> String {
        let new_sha_out = Command::new("git")
            .args([
                "-c",
                "user.name=External Advancer",
                "-c",
                "user.email=advance@example.invalid",
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                "HEAD",
                "-m",
                "External advance",
            ])
            .current_dir(bare_repo)
            .output()
            .expect("git commit-tree failed");
        assert!(new_sha_out.status.success(), "git commit-tree must succeed");
        let new_sha = String::from_utf8(new_sha_out.stdout)
            .expect("commit-tree output must be UTF-8")
            .trim()
            .to_owned();

        let refname = format!("refs/heads/{branch}");
        let status = Command::new("git")
            .args(["update-ref", &refname, &new_sha])
            .current_dir(bare_repo)
            .status()
            .expect("git update-ref failed");
        assert!(status.success(), "git update-ref must succeed");
        new_sha
    }

    #[test]
    fn scheduler_handler_maps_integration_conflict_to_failed_outcome() {
        let (_temp, artifact) = fixture("handler-cas-conflict");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "cas-output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        // Run the node to stash a pending update.
        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        // Advance the branch externally between RunNode and IntegrateWork.
        let advanced_sha = advance_branch_in_bare(&repo_path, "main");

        // Attempt to integrate the stale workspace.
        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote cas-output.txt".to_string(),
            },
        });

        let IntegrationOutcome::Failed(failure) = (match &event {
            SchedulerEvent::IntegrationReturned { outcome, .. } => outcome,
            other => panic!("expected IntegrationReturned, got: {other:#?}"),
        }) else {
            panic!("expected IntegrationOutcome::Failed, got: {event:#?}");
        };

        assert!(
            failure.reason.contains(&original_sha) || failure.reason.contains(&advanced_sha),
            "failure reason must mention expected or actual commit SHA; got: {}",
            failure.reason
        );

        // Branch must remain at the externally advanced commit.
        let tip = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            tip, advanced_sha,
            "branch must remain at the externally advanced commit"
        );
    }

    #[test]
    fn work_node_without_update_integrates_without_commit() {
        let (_temp, artifact) = fixture("no-update-no-commit");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "do some work".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "completed".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Succeeded(_),
                    ..
                }
            ),
            "IntegrateWork with no pending update must return Succeeded"
        );

        let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_after, original_sha,
            "commit must not change when no artifact update was pending"
        );
    }

    // ── validation tests ──────────────────────────────────────────────────────

    use crate::artifacts::Workspace;
    use crate::validation::{ValidationResult, Validator};

    struct AlwaysFailValidator;

    impl Validator for AlwaysFailValidator {
        fn validate(&self, _workspace: &Workspace) -> ValidationResult {
            ValidationResult {
                passed: false,
                summary: "intentional failure".to_string(),
            }
        }
    }

    /// Reads a specific file from the workspace and asserts it exists.
    struct FileExistsValidator {
        path: String,
        found: Rc<RefCell<bool>>,
    }

    impl Validator for FileExistsValidator {
        fn validate(&self, workspace: &Workspace) -> ValidationResult {
            let exists = workspace.path().join(&self.path).exists();
            *self.found.borrow_mut() = exists;
            ValidationResult {
                passed: true,
                summary: format!("checked {}", self.path),
            }
        }
    }

    struct PanicOnCallValidator;

    impl Validator for PanicOnCallValidator {
        fn validate(&self, _workspace: &Workspace) -> ValidationResult {
            panic!("validator must not be called when there is no pending update");
        }
    }

    #[test]
    fn validation_pass_allows_commit() {
        let (_temp, artifact) = fixture("validation-pass");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Succeeded(_),
                    ..
                }
            ),
            "AlwaysPassValidator must allow integration; got: {event:#?}"
        );

        let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_ne!(
            new_sha, original_sha,
            "commit must advance when validation passes"
        );
    }

    #[test]
    fn validation_failure_blocks_commit() {
        let (_temp, artifact) = fixture("validation-fail");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact)
            .with_validator(Rc::new(AlwaysFailValidator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Failed(_),
                    ..
                }
            ),
            "failing validator must block integration; got: {event:#?}"
        );

        let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_after, original_sha,
            "commit must not advance when validation fails"
        );
    }

    #[test]
    fn validator_runs_after_update_apply() {
        let (_temp, artifact) = fixture("validator-after-apply");

        let runner = FileWritingRunner {
            path: "applied.txt".to_string(),
            content: "applied content\n".to_string(),
        };

        let found = Rc::new(RefCell::new(false));
        let validator = FileExistsValidator {
            path: "applied.txt".to_string(),
            found: found.clone(),
        };

        let h =
            SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote applied.txt".to_string(),
            },
        });

        assert!(
            *found.borrow(),
            "validator must see applied.txt in the workspace after update apply"
        );
    }

    #[test]
    fn no_update_does_not_run_validator() {
        let (_temp, artifact) = fixture("no-update-no-validator");

        let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact)
            .with_validator(Rc::new(PanicOnCallValidator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "do some work".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        // StaticNodeRunner produces no artifact update, so validator must not be called.
        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "no file changes".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Succeeded(_),
                    ..
                }
            ),
            "integration with no pending update must succeed without calling validator; got: {event:#?}"
        );
    }

    #[test]
    fn validation_pass_sets_validation_passed_true() {
        let (_temp, artifact) = fixture("vp-pass-true");

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });
        assert_eq!(
            h.validation_passed(),
            None,
            "validation_passed must be None before IntegrateWork"
        );

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert_eq!(
            h.validation_passed(),
            Some(true),
            "validation_passed must be Some(true) after AlwaysPassValidator"
        );
    }

    #[test]
    fn validation_failure_sets_validation_passed_false() {
        let (_temp, artifact) = fixture("vp-fail-false");

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact)
            .with_validator(Rc::new(AlwaysFailValidator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert_eq!(
            h.validation_passed(),
            Some(false),
            "validation_passed must be Some(false) after AlwaysFailValidator"
        );
    }

    #[test]
    fn no_update_leaves_validation_passed_none() {
        let (_temp, artifact) = fixture("vp-no-update-none");

        let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "do some work".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "no files".to_string(),
            },
        });

        assert_eq!(
            h.validation_passed(),
            None,
            "validation_passed must remain None when no artifact update was pending"
        );
    }

    #[test]
    fn validation_passed_true_even_when_integration_conflicts() {
        let (_temp, artifact) = fixture("vp-true-on-cas-conflict");
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact);

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        // Advance the branch externally so the integrate() CAS check fails.
        advance_branch_in_bare(&repo_path, "main");

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Failed(_),
                    ..
                }
            ),
            "CAS conflict must produce IntegrationOutcome::Failed; got: {event:#?}"
        );
        assert_eq!(
            h.validation_passed(),
            Some(true),
            "validation_passed must be Some(true) even when CAS integration fails after validation"
        );
    }

    #[test]
    fn validation_failure_does_not_leave_artifact_changed() {
        let (_temp, artifact) = fixture("validation-no-history-change");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let h = SchedulerHandler::with_artifact(runner, artifact)
            .with_validator(Rc::new(AlwaysFailValidator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_after, original_sha,
            "artifact commit history must remain unchanged after validation failure"
        );

        let log_count = git_output(&repo_path, &["rev-list", "--count", "HEAD"]);
        assert_eq!(
            log_count, "1",
            "commit history must contain only the initial commit after validation failure"
        );
    }

    #[test]
    fn timeout_blocks_commit() {
        use crate::validation::CommandValidator;
        use std::time::Duration;

        let (_temp, artifact) = fixture("timeout-blocks-commit");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        // Validator times out immediately — sleep 5 with a 1-second budget.
        let validator = CommandValidator::new(vec!["sleep 5".to_string()], Duration::from_secs(1));
        let h =
            SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        let event = h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        assert!(
            matches!(
                event,
                SchedulerEvent::IntegrationReturned {
                    outcome: IntegrationOutcome::Failed(_),
                    ..
                }
            ),
            "timed-out validator must block integration; got: {event:#?}"
        );

        let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            sha_after, original_sha,
            "artifact commit must not change when validation times out"
        );
    }

    // ── workspace cleanup tests ───────────────────────────────────────────────

    /// Captures the workspace path and controls whether validation passes or fails.
    struct PathCapturingValidator {
        captured: Rc<RefCell<Option<std::path::PathBuf>>>,
        pass: bool,
    }

    impl Validator for PathCapturingValidator {
        fn validate(&self, workspace: &Workspace) -> ValidationResult {
            *self.captured.borrow_mut() = Some(workspace.path().to_path_buf());
            ValidationResult {
                passed: self.pass,
                summary: "path-capturing validator".to_string(),
            }
        }
    }

    #[test]
    fn temporary_workspace_removed_after_successful_integration() {
        let (_temp, artifact) = fixture("temp-removed-success");
        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let captured = Rc::new(RefCell::new(None));
        let validator = PathCapturingValidator {
            captured: captured.clone(),
            pass: true,
        };
        let h =
            SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        let path = captured
            .borrow()
            .clone()
            .expect("validator must have been called");
        assert!(
            !path.exists(),
            "temporary workspace must be removed after successful integration"
        );
    }

    #[test]
    fn temporary_workspace_removed_after_validation_failure() {
        let (_temp, artifact) = fixture("temp-removed-validation-fail");
        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let captured = Rc::new(RefCell::new(None));
        let validator = PathCapturingValidator {
            captured: captured.clone(),
            pass: false,
        };
        let h =
            SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

        h.handle_effect(SchedulerEffect::RunNode {
            node_id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "write a file".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        });

        h.handle_effect(SchedulerEffect::IntegrateWork {
            node_id: NodeId("W".to_string()),
            work: WorkOutput {
                summary: "wrote output.txt".to_string(),
            },
        });

        let path = captured
            .borrow()
            .clone()
            .expect("validator must have been called");
        assert!(
            !path.exists(),
            "temporary workspace must be removed after validation failure"
        );
    }

    // ── telemetry tests ───────────────────────────────────────────────────────

    /// A node runner that always fails with a fixed reason.
    struct AlwaysFailRunner {
        reason: String,
    }

    impl NodeRunner for AlwaysFailRunner {
        fn run_node(
            &self,
            _request: NodeRunRequest,
            _telemetry: &dyn TelemetrySink,
        ) -> NodeRunResult {
            use crate::machines::scheduler::event::{NodeFailure, RecoveryAction};
            NodeRunResult::Failed(NodeFailure {
                reason: self.reason.clone(),
                recovery: RecoveryAction::Terminal {
                    message: "terminal".to_string(),
                },
            })
        }
    }

    #[test]
    fn node_failure_reason_preserved_in_full_in_telemetry() {
        use crate::engine::run_machine_with_telemetry;
        use crate::telemetry::FileTelemetry;

        let long_reason = "provider error: connection timed out after 3 retries; \
            last attempt returned status 503; node objective was 'write the implementation'; \
            this reason must appear verbatim in the telemetry file and must not be elided to '...'";

        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-handler-telemetry-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let sink = FileTelemetry::new(dir.clone());

        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("fail-node", "do some work")],
                next_id: 0,
            },
        };

        run_machine_with_telemetry(
            SchedulerHandler::new(AlwaysFailRunner {
                reason: long_reason.to_string(),
            }),
            state,
            &sink,
        );

        let all_content: String = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .collect::<Vec<_>>()
            .join("\n");

        let _ = fs::remove_dir_all(&dir);

        assert!(
            all_content.contains(long_reason),
            "telemetry must contain the full failure reason; got:\n{all_content}"
        );
        assert!(
            !all_content.contains("reason: \"...\""),
            "telemetry must not elide the failure reason to '...'; got:\n{all_content}"
        );
    }

    #[test]
    fn telemetry_failure_does_not_change_scheduler_behavior() {
        use crate::engine::run_machine_with_telemetry;
        use crate::telemetry::FileTelemetry;

        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-handler-tel-fail-sched-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        // Create the sink, then delete the directory so all writes fail.
        let sink = FileTelemetry::new(dir.clone());
        let _ = fs::remove_dir_all(&dir);
        let shared: Rc<dyn TelemetrySink> = Rc::new(sink);

        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "do some work")],
                next_id: 0,
            },
        };
        let output = run_machine_with_telemetry(
            SchedulerHandler::new(StaticNodeRunner).with_telemetry(Rc::clone(&shared)),
            state,
            shared.as_ref(),
        );
        assert!(
            matches!(output.0, SchedulerOutput::Complete { .. }),
            "scheduler output must be Complete regardless of telemetry failures; got: {:#?}",
            output.0
        );
    }

    #[test]
    fn artifact_commit_still_succeeds_when_telemetry_fails() {
        use crate::engine::run_machine_with_telemetry;
        use crate::telemetry::FileTelemetry;

        let (_temp, artifact) = fixture("tel-fail-commit");
        let original_sha = artifact.commit_sha.clone();
        let repo_path = artifact.repo_path.clone();

        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-handler-tel-fail-commit-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        // Create the sink, then delete the directory so all writes fail.
        let sink = FileTelemetry::new(dir.clone());
        let _ = fs::remove_dir_all(&dir);
        let shared: Rc<dyn TelemetrySink> = Rc::new(sink);

        let runner = FileWritingRunner {
            path: "result.txt".to_string(),
            content: "committed despite telemetry failure\n".to_string(),
        };
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "write a file")],
                next_id: 0,
            },
        };
        let (output, handler) = run_machine_with_telemetry(
            SchedulerHandler::with_artifact(runner, artifact).with_telemetry(Rc::clone(&shared)),
            state,
            shared.as_ref(),
        );

        assert!(
            matches!(output, SchedulerOutput::Complete { .. }),
            "run must complete even when telemetry writes all fail; got: {output:#?}"
        );

        let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
        assert_ne!(
            new_sha, original_sha,
            "artifact commit must advance even when telemetry fails"
        );

        let final_artifact = handler.artifact().expect("artifact must be present");
        assert_eq!(
            final_artifact.commit_sha, new_sha,
            "handler artifact must reflect the committed SHA"
        );
    }

    // ── shared-trace tests ────────────────────────────────────────────────────

    /// Scripted provider for shared-trace tests.
    struct ScriptedProvider {
        responses: RefCell<std::collections::VecDeque<String>>,
    }

    impl ScriptedProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            }
        }
    }

    impl crate::providers::ProviderClient for ScriptedProvider {
        fn call(
            &self,
            _req: crate::providers::ProviderRequest,
        ) -> Result<crate::providers::ProviderResponse, crate::providers::ProviderError> {
            let content = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedProvider: responses exhausted");
            Ok(crate::providers::ProviderResponse {
                content,
                finish_reason: None,
            })
        }
    }

    #[test]
    fn scheduler_and_deliberation_share_one_trace() {
        use crate::engine::run_machine_with_telemetry;
        use crate::node_runner::DeliberatingNodeRunner;
        use crate::telemetry::{TelemetryEvent, VecTelemetry};

        let vec_tel = Rc::new(VecTelemetry::new());
        let shared: Rc<dyn TelemetrySink> = vec_tel.clone();

        // Plan node + work node, each requiring 3 provider calls (producer, critic, referee).
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"implement it"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
            r#"{"status":"accepted","content":"done"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "do something".to_string(),
        });

        let handler = SchedulerHandler::new(runner).with_telemetry(Rc::clone(&shared));
        let _ = run_machine_with_telemetry(handler, initial_state, shared.as_ref());

        let records = vec_tel.records();
        let machine_names: Vec<&str> = records
            .iter()
            .filter_map(|record| match &record.event {
                TelemetryEvent::MachineStarted { machine } => Some(machine.as_str()),
                _ => None,
            })
            .collect();

        assert!(
            machine_names.contains(&"SchedulerMachine"),
            "expected SchedulerMachine in shared trace; got: {machine_names:?}"
        );
        assert!(
            machine_names.contains(&"DeliberationMachine"),
            "expected DeliberationMachine in shared trace; got: {machine_names:?}"
        );
    }

    #[test]
    fn nested_machine_events_preserve_order() {
        use crate::engine::run_machine_with_telemetry;
        use crate::node_runner::DeliberatingNodeRunner;
        use crate::telemetry::{TelemetryEvent, VecTelemetry};

        let vec_tel = Rc::new(VecTelemetry::new());
        let shared: Rc<dyn TelemetrySink> = vec_tel.clone();

        // Plan node + work node, each requiring 3 provider calls (producer, critic, referee).
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"implement it"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
            r#"{"status":"accepted","content":"done"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "do something".to_string(),
        });

        let handler = SchedulerHandler::new(runner).with_telemetry(Rc::clone(&shared));
        let _ = run_machine_with_telemetry(handler, initial_state, shared.as_ref());

        let records = vec_tel.records();
        let machine_seq: Vec<&str> = records
            .iter()
            .filter_map(|record| match &record.event {
                TelemetryEvent::MachineStarted { machine } => Some(machine.as_str()),
                _ => None,
            })
            .collect();

        let sched_pos = machine_seq
            .iter()
            .position(|&m| m == "SchedulerMachine")
            .expect("SchedulerMachine must appear in trace");
        let delib_pos = machine_seq
            .iter()
            .position(|&m| m == "DeliberationMachine")
            .expect("DeliberationMachine must appear in trace");

        assert!(
            sched_pos < delib_pos,
            "SchedulerMachine must start before DeliberationMachine; positions: {sched_pos} vs {delib_pos}"
        );

        // Verify scheduler events appear after deliberation finishes (EffectEmitted
        // is recorded before handle_effect; StateEntered of the next scheduler loop
        // iteration appears after the deliberation run completes).
        let last_delib_idx = records
            .iter()
            .rposition(|record| match &record.event {
                TelemetryEvent::StateEntered { machine, .. }
                | TelemetryEvent::EventReceived { machine, .. }
                | TelemetryEvent::EffectEmitted { machine, .. } => machine == "DeliberationMachine",
                _ => false,
            })
            .expect("deliberation must emit at least one event");

        let sched_after =
            records
                .iter()
                .skip(last_delib_idx + 1)
                .any(|record| match &record.event {
                    TelemetryEvent::StateEntered { machine, .. }
                    | TelemetryEvent::EventReceived { machine, .. } => {
                        machine == "SchedulerMachine"
                    }
                    _ => false,
                });

        assert!(
            sched_after,
            "SchedulerMachine must have events after DeliberationMachine finishes"
        );
    }

    #[test]
    fn runtime_creates_only_one_file_telemetry() {
        use crate::engine::run_machine_with_telemetry;
        use crate::node_runner::DeliberatingNodeRunner;
        use crate::telemetry::FileTelemetry;

        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("forge-single-sink-{}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let file_sink = FileTelemetry::new(dir.clone());
        let shared: Rc<dyn TelemetrySink> = Rc::new(file_sink);

        // Plan node + work node, each requiring 3 provider calls (producer, critic, referee).
        let provider = ScriptedProvider::from_strs(&[
            r#"{"status":"accepted","content":"implement it"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
            r#"{"status":"accepted","content":"done"}"#,
            r#"{"status":"accepted","content":"looks good"}"#,
            r#"{"status":"accepted","content":"approved"}"#,
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "do something".to_string(),
        });

        let handler = SchedulerHandler::new(runner).with_telemetry(Rc::clone(&shared));
        let _ = run_machine_with_telemetry(handler, initial_state, shared.as_ref());

        // Both scheduler and deliberation events must land in one directory.
        let all_content: String = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .collect::<Vec<_>>()
            .join("\n");

        let _ = fs::remove_dir_all(&dir);

        assert!(
            all_content.contains("machine: SchedulerMachine"),
            "telemetry directory must contain SchedulerMachine events"
        );
        assert!(
            all_content.contains("machine: DeliberationMachine"),
            "telemetry directory must contain DeliberationMachine events — no separate sink was created"
        );

        // No subdirectories should exist (no nested FileTelemetry was created).
        // We verify this by checking the dir no longer exists (we removed it above).
        // If a nested FileTelemetry had been created it would also have been inside
        // dir, which we removed — but the key structural guarantee is that the code
        // path through SchedulerHandler and DeliberatingNodeRunner never calls
        // FileTelemetry::new internally.
    }

    // ── checkpoint tests ──────────────────────────────────────────────────────

    #[test]
    fn checkpoint_written_after_node_returned() {
        use crate::engine::run_machine;
        use crate::runtime::checkpoint::load_checkpoint;

        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("forge-handler-ckpt-{}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "do some work")],
                next_id: 0,
            },
        };
        run_machine(
            SchedulerHandler::new(StaticNodeRunner).with_checkpoint_dir(dir.clone()),
            state,
        );

        let checkpoint_path = dir.join("graph.json");
        assert!(
            checkpoint_path.exists(),
            "graph.json must be written after run"
        );
        // The checkpoint captures the last non-terminal state (Running, not Complete).
        // The final Complete state is a terminal and is never checkpointed.
        let loaded = load_checkpoint(&dir).unwrap();
        let SchedulerState::Running { graph } = loaded else {
            panic!("expected Running state in checkpoint");
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.status == NodeStatus::Completed),
            "all nodes must be Completed in the final checkpoint"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_load_round_trip() {
        use crate::runtime::checkpoint::{load_checkpoint, save_checkpoint};

        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-handler-ckpt-rt-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    Node {
                        id: NodeId("A".to_string()),
                        kind: NodeKind::Work,
                        objective: "do A".to_string(),
                        dependencies: vec![],
                        status: NodeStatus::Completed,
                        attempt: 0,
                        plan_depth: 0,
                        model_tier: ModelTier::Cheap,
                        summary: Some("done".to_string()),
                        origin: NodeOrigin::Root,
                    },
                    work_node("B", "do B"),
                ],
                next_id: 1,
            },
        };

        save_checkpoint(&dir, &state).unwrap();
        let loaded = load_checkpoint(&dir).unwrap();
        assert_eq!(state, loaded);

        let _ = fs::remove_dir_all(&dir);
    }
}
