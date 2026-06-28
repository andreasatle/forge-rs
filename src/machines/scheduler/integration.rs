//! Artifact update staging and integration for scheduler work nodes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::artifacts::{Artifact, ArtifactUpdate, create_temporary_workspace, integrate};
use crate::machines::scheduler::event::{
    FailureKind, IntegrationFailure, IntegrationOutcome, IntegrationOutput, RecoveryAction,
    SchedulerEvent, WorkOutput,
};
use crate::machines::scheduler::state::NodeId;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::validation::{AlwaysPassValidator, ValidationPlan, ValidationResult, Validator};

use super::validation::validation_retry_message;

pub(crate) struct IntegrationService {
    artifact: RefCell<Option<Artifact>>,
    pending_artifact_updates: RefCell<HashMap<NodeId, ArtifactUpdate>>,
    validator: Rc<dyn Validator>,
    last_validation_passed: RefCell<Option<bool>>,
    telemetry: Rc<dyn TelemetrySink>,
}

impl IntegrationService {
    pub(crate) fn without_artifact(telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self {
            artifact: RefCell::new(None),
            pending_artifact_updates: RefCell::new(HashMap::new()),
            validator: Rc::new(AlwaysPassValidator),
            last_validation_passed: RefCell::new(None),
            telemetry,
        }
    }

    pub(crate) fn with_artifact(artifact: Artifact, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self {
            artifact: RefCell::new(Some(artifact)),
            pending_artifact_updates: RefCell::new(HashMap::new()),
            validator: Rc::new(AlwaysPassValidator),
            last_validation_passed: RefCell::new(None),
            telemetry,
        }
    }

    pub(crate) fn with_validator(self, validator: Rc<dyn Validator>) -> Self {
        Self { validator, ..self }
    }

    pub(crate) fn with_telemetry(self, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self { telemetry, ..self }
    }

    pub(crate) fn artifact(&self) -> Option<Artifact> {
        self.artifact.borrow().clone()
    }

    pub(crate) fn validation_passed(&self) -> Option<bool> {
        *self.last_validation_passed.borrow()
    }

    pub(crate) fn stage_update(&self, node_id: NodeId, update: ArtifactUpdate) {
        self.pending_artifact_updates
            .borrow_mut()
            .insert(node_id, update);
    }

    pub(crate) fn integrate_work(
        &self,
        node_id: NodeId,
        work: WorkOutput,
        target_files: Vec<String>,
        validation_plan: Option<ValidationPlan>,
    ) -> SchedulerEvent {
        eprintln!("[integration] start {}", node_id.0);

        let pending_update = self.pending_artifact_updates.borrow_mut().remove(&node_id);
        let artifact_snapshot = self.artifact.borrow().clone();

        if let (Some(update), Some(artifact)) = (pending_update, artifact_snapshot) {
            let workspace_result = create_temporary_workspace(&artifact);
            let mut workspace = match workspace_result {
                Ok(w) => w,
                Err(err) => {
                    return SchedulerEvent::IntegrationReturned {
                        node_id,
                        outcome: integration_failure(format!("workspace creation failed: {err}")),
                    };
                }
            };

            let changed_files = update.changed_paths();
            match update.apply(&mut workspace) {
                Err(err) => {
                    return SchedulerEvent::IntegrationReturned {
                        node_id,
                        outcome: integration_failure(format!("artifact update apply error: {err}")),
                    };
                }
                Ok(()) => {
                    self.telemetry.record(TelemetryRecord::new(
                        "Integration",
                        TelemetryEvent::ValidationStarted,
                    ));
                    let result = run_validation(
                        &workspace,
                        validation_plan.as_ref(),
                        &*self.validator,
                        &target_files,
                        &changed_files,
                    );
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
                                    outcome: integration_failure(err.to_string()),
                                };
                            }
                        }
                    } else {
                        *self.last_validation_passed.borrow_mut() = Some(false);
                        let diagnostic_message =
                            validation_retry_message(&result.summary, result.failure.as_ref());
                        self.telemetry.record(TelemetryRecord::new(
                            "Integration",
                            TelemetryEvent::ValidationFailed {
                                summary: result.summary.clone(),
                            },
                        ));
                        return SchedulerEvent::IntegrationReturned {
                            node_id,
                            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                                kind: FailureKind::ValidationFailure,
                                message: diagnostic_message.clone(),
                                recovery: RecoveryAction::Retry {
                                    message: diagnostic_message,
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
}

/// Run validation using the node's `ValidationPlan` when present, falling back
/// to the handler-level `Validator` singleton otherwise.
fn run_validation(
    workspace: &crate::artifacts::Workspace,
    plan: Option<&ValidationPlan>,
    fallback: &dyn Validator,
    target_files: &[String],
    changed_files: &[String],
) -> ValidationResult {
    match plan {
        Some(p) => p.execute_scoped(workspace, target_files, changed_files),
        None => fallback.validate(workspace),
    }
}

fn integration_failure(message: String) -> IntegrationOutcome {
    IntegrationOutcome::Failed(IntegrationFailure {
        kind: FailureKind::IntegrationFailure,
        message: message.clone(),
        recovery: RecoveryAction::Terminal { message },
    })
}
