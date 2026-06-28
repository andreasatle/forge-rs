//! Effect handler for the scheduler machine.
//!
//! [`SchedulerHandler`] implements [`Machine`] by delegating pure transition
//! logic to [`SchedulerMachine`] and routing side effects through focused
//! scheduler services.

use std::path::PathBuf;
use std::rc::Rc;

use crate::artifacts::Artifact;
use crate::engine::{Machine, Transition};
use crate::machines::scheduler::checkpoint::CheckpointService;
use crate::machines::scheduler::dispatch::{RunNodeDispatch, dispatch_run_node};
use crate::machines::scheduler::effect::SchedulerEffect;
use crate::machines::scheduler::event::SchedulerEvent;
use crate::machines::scheduler::integration::IntegrationService;
use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
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
    telemetry: Rc<dyn TelemetrySink>,
    /// Forwarded to `SchedulerMachine` to control `ElevateModel` recovery policy.
    has_strong_tier: bool,
}

impl<R: NodeRunner> SchedulerHandler<R> {
    /// Create a new handler backed by the given [`NodeRunner`], with no artifact.
    pub fn new(runner: R) -> Self {
        let telemetry: Rc<dyn TelemetrySink> = Rc::new(NoopTelemetry);
        Self {
            runner,
            integration: IntegrationService::without_artifact(Rc::clone(&telemetry)),
            checkpoint: CheckpointService::disabled(Rc::clone(&telemetry)),
            telemetry,
            has_strong_tier: true,
        }
    }

    /// Create a handler that owns an [`Artifact`] and keeps it current across
    /// work node integrations.
    pub fn with_artifact(runner: R, artifact: Artifact) -> Self {
        let telemetry: Rc<dyn TelemetrySink> = Rc::new(NoopTelemetry);
        Self {
            runner,
            integration: IntegrationService::with_artifact(artifact, Rc::clone(&telemetry)),
            checkpoint: CheckpointService::disabled(Rc::clone(&telemetry)),
            telemetry,
            has_strong_tier: true,
        }
    }

    /// Set whether a distinct strong-tier model is configured.
    ///
    /// When `false`, `ElevateModel` recovery is demoted to `Retry` (or `Terminal`
    /// when attempts are exhausted). Defaults to `true`.
    pub fn with_has_strong_tier(self, has_strong_tier: bool) -> Self {
        Self {
            has_strong_tier,
            ..self
        }
    }

    /// Attach a shared telemetry sink so node runs record into the same trace.
    pub fn with_telemetry(self, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self {
            integration: self.integration.with_telemetry(Rc::clone(&telemetry)),
            checkpoint: self.checkpoint.with_telemetry(Rc::clone(&telemetry)),
            telemetry,
            ..self
        }
    }

    /// Replace the default [`crate::validation::AlwaysPassValidator`] with a
    /// custom validator.
    pub fn with_validator(self, validator: Rc<dyn Validator>) -> Self {
        Self {
            integration: self.integration.with_validator(validator),
            ..self
        }
    }

    /// Enable checkpoint writes to `dir` after each progress event.
    ///
    /// When set, the handler writes `graph.json` to `dir` after every
    /// `NodeReturned` and `IntegrationReturned` transition that leaves the
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

impl<R: NodeRunner> Machine for SchedulerHandler<R> {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerOutput;

    fn name(&self) -> String {
        "SchedulerMachine".to_string()
    }

    fn start_event(&self) -> SchedulerEvent {
        SchedulerMachine {
            has_strong_tier: self.has_strong_tier,
        }
        .start_event()
    }

    fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        print_returned_progress(&event);
        let should_checkpoint = is_progress_event(&event);
        let result = SchedulerMachine {
            has_strong_tier: self.has_strong_tier,
        }
        .transition(state, event);
        if should_checkpoint {
            self.checkpoint.maybe_save(&result.state);
        }
        result
    }

    fn handle_effect(&self, effect: SchedulerEffect) -> SchedulerEvent {
        match effect {
            SchedulerEffect::RunNode {
                node_id,
                kind,
                objective,
                target_files,
                model_tier,
                attempt,
            } => {
                let command = RunNodeDispatch {
                    node_id,
                    kind,
                    objective,
                    target_files,
                    model_tier,
                    attempt,
                };
                let result = dispatch_run_node(
                    &self.runner,
                    self.telemetry.as_ref(),
                    command,
                    self.integration.artifact(),
                );
                if let SchedulerEvent::NodeReturned { node_id, .. } = &result.event
                    && let Some(update) = result.artifact_update
                {
                    self.integration.stage_update(node_id.clone(), update);
                }
                result.event
            }
            SchedulerEffect::IntegrateWork { node_id, work } => {
                self.integration.integrate_work(node_id, work)
            }
            SchedulerEffect::ReturnComplete { .. } | SchedulerEffect::ReturnFailed { .. } => {
                unreachable!("return effects are never dispatched to the effect handler")
            }
        }
    }

    fn output(&self, state: &SchedulerState) -> Option<SchedulerOutput> {
        SchedulerMachine {
            has_strong_tier: self.has_strong_tier,
        }
        .output(state)
    }
}
