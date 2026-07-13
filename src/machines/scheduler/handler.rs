//! Effect handler for the scheduler machine.
//!
//! [`SchedulerHandler`] executes [`SchedulerEffect`] values and, for
//! progress-carrying events, wraps [`SchedulerMachine`]'s pure transition with
//! checkpoint persistence. It does not implement the generic
//! [`Machine`](crate::engine::Machine) trait itself; a private driver
//! composes this handler with [`SchedulerMachine`] into something
//! [`crate::engine::run_machine`] can drive.

use std::path::PathBuf;
use std::sync::Arc;

use crate::artifacts::Artifact;
use crate::engine::Transition;
use crate::machines::scheduler::checkpoint::CheckpointService;
use crate::machines::scheduler::dispatch::{RunNodeDispatch, dispatch_run_node};
use crate::machines::scheduler::effect::SchedulerEffect;
use crate::machines::scheduler::event::SchedulerEvent;
use crate::machines::scheduler::integration::{IntegrationService, WorkIntegration};
use crate::machines::scheduler::machine::SchedulerMachine;
use crate::machines::scheduler::progress::{is_progress_event, print_returned_progress};
use crate::machines::scheduler::state::SchedulerState;
use crate::node_runner::NodeRunner;
use crate::telemetry::{NoopTelemetry, TelemetrySink};
use crate::validation::Validator;

/// Drives the scheduler machine using a [`NodeRunner`] to execute nodes.
///
/// All pure transition logic is delegated to [`SchedulerMachine`]. This type
/// owns effect orchestration and delegates the cohesive side-effect work to:
///
/// - node dispatch,
/// - artifact update staging and integration,
/// - checkpoint persistence, and
/// - progress reporting.
pub struct SchedulerHandler<R> {
    runner: R,
    integration: IntegrationService,
    checkpoint: CheckpointService,
    telemetry: Arc<dyn TelemetrySink>,
}

impl<R: NodeRunner> SchedulerHandler<R> {
    /// Create a new handler backed by the given [`NodeRunner`], with no artifact.
    pub fn new(runner: R) -> Self {
        let telemetry: Arc<dyn TelemetrySink> = Arc::new(NoopTelemetry);
        Self {
            runner,
            integration: IntegrationService::without_artifact(Arc::clone(&telemetry)),
            checkpoint: CheckpointService::disabled(Arc::clone(&telemetry)),
            telemetry,
        }
    }

    /// Create a handler that owns an [`Artifact`] and keeps it current across
    /// work node integrations.
    pub fn with_artifact(runner: R, artifact: Artifact) -> Self {
        let telemetry: Arc<dyn TelemetrySink> = Arc::new(NoopTelemetry);
        Self {
            runner,
            integration: IntegrationService::with_artifact(artifact, Arc::clone(&telemetry)),
            checkpoint: CheckpointService::disabled(Arc::clone(&telemetry)),
            telemetry,
        }
    }

    /// Attach a shared telemetry sink so node runs record into the same trace.
    pub fn with_telemetry(self, telemetry: Arc<dyn TelemetrySink>) -> Self {
        Self {
            integration: self.integration.with_telemetry(Arc::clone(&telemetry)),
            checkpoint: self.checkpoint.with_telemetry(Arc::clone(&telemetry)),
            telemetry,
            ..self
        }
    }

    /// Replace the default [`crate::validation::AlwaysPassValidator`] with a
    /// custom validator.
    pub fn with_validator(self, validator: Arc<dyn Validator>) -> Self {
        Self {
            integration: self.integration.with_validator(validator),
            ..self
        }
    }

    /// Enable checkpoint writes to `dir` after each progress event.
    ///
    /// When set, the handler writes `graph.json` to `dir` after every
    /// node and integration completion transition that leaves the
    /// scheduler in a non-terminal state.
    pub fn with_checkpoint_dir(self, dir: PathBuf) -> Self {
        Self {
            checkpoint: self.checkpoint.with_dir(dir),
            ..self
        }
    }

    /// Returns a clone of the current artifact, or `None` if no artifact was provided.
    pub fn artifact(&self) -> Option<Artifact> {
        self.integration.artifact()
    }

    /// Returns whether the integration validation gate ran and what it returned.
    ///
    /// `Some(true)` means validation ran and passed (even if CAS integration later failed).
    /// `Some(false)` means validation ran and failed.
    /// `None` means the gate was never reached (no artifact update was pending).
    pub fn validation_passed(&self) -> Option<bool> {
        self.integration.validation_passed()
    }
}

impl<R: NodeRunner> SchedulerHandler<R> {
    /// Delegates to `SchedulerMachine`'s pure transition, then persists a
    /// checkpoint when `event` carries progress (a node or integration
    /// result). Progress reporting and checkpointing are handler-owned
    /// orchestration, not part of the pure transition itself.
    pub(crate) fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        print_returned_progress(&event);
        let should_checkpoint = is_progress_event(&event);
        let result = SchedulerMachine.transition(state, event);
        if should_checkpoint {
            self.checkpoint.maybe_save(&result.state);
        }
        result
    }

    /// Executes one effect and converts the external result into the next event.
    pub(crate) fn handle_effect(&self, effect: SchedulerEffect) -> SchedulerEvent {
        match effect {
            SchedulerEffect::RunNode {
                node_id,
                worker_role,
                kind,
                objective,
                target_files,
                test_plan_context,
                model_tier,
                attempt,
                retry_feedback,
                team,
                adapter,
                northstar,
            } => {
                let command = RunNodeDispatch {
                    node_id: node_id.clone(),
                    worker_role,
                    kind,
                    objective,
                    target_files,
                    test_plan_context,
                    model_tier,
                    attempt,
                    retry_feedback,
                    team,
                    adapter,
                    northstar,
                };
                let work_attempt = if command.kind == crate::machines::scheduler::NodeKind::Work {
                    self.integration
                        .prepare_work_attempt(command.node_id.clone(), command.attempt)
                } else {
                    None
                };
                let result = dispatch_run_node(
                    &self.runner,
                    self.telemetry.as_ref(),
                    command,
                    self.integration.artifact(),
                    work_attempt.clone(),
                );
                if let Some((node_id, reason)) = node_rejection_reason(&result.event)
                    && let Some(attempt) = work_attempt
                {
                    self.integration.discard_work_attempt_with_reason(
                        node_id,
                        attempt.attempt,
                        reason,
                    );
                }
                result.event
            }
            SchedulerEffect::IntegrateWork {
                node_id,
                objective,
                work,
                attempt,
                target_files,
                validation_plan,
                team,
                task_id,
            } => self.integration.integrate_work(WorkIntegration {
                node_id,
                objective,
                work,
                attempt,
                target_files,
                validation_plan,
                team,
                task_id,
            }),
            SchedulerEffect::IntegratePlannerTasks {
                node_id,
                tasks,
                team,
            } => self.integration.integrate_plan_tasks(node_id, tasks, team),
        }
    }
}

fn node_rejection_reason(
    event: &SchedulerEvent,
) -> Option<(&crate::machines::scheduler::NodeId, String)> {
    match event {
        SchedulerEvent::WorkAccepted { .. } => None,
        SchedulerEvent::NodeFailed { node_id, failure } => Some((node_id, failure.message.clone())),
        SchedulerEvent::PlanAccepted { node_id, .. } => Some((
            node_id,
            format!("work attempt rejected with event: {event:#?}"),
        )),
        _ => None,
    }
}
