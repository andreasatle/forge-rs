use crate::artifacts::{ArtifactUpdate, FileChange};
use crate::machines::deliberation::state::DeliberationRole;
use crate::machines::scheduler::{FailureKind, NodeKind};
use crate::roles::runner::{RoleRequest, RoleRunner};
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;
use super::handler::{
    DeliberationHandler, MAX_PLAN_VALIDATION_RETRIES, MAX_WORK_SEMANTIC_VALIDATION_RETRIES,
};
use super::semantic_validation::ProducerSemanticValidationConfig;

impl<R: RoleRunner> DeliberationHandler<R> {
    /// Execute one deliberation effect and record role-layer protocol telemetry.
    pub fn handle_effect_with_telemetry(
        &self,
        effect: DeliberationEffect,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        match effect {
            DeliberationEffect::RunRole {
                role,
                objective,
                target_files,
                producer_content,
                critic_content,
                feedback,
            } => {
                let (tool_context, target_views) =
                    match self.role_tool_context_and_target_views(&role, &target_files) {
                        Ok(context) => context,
                        Err(error) => {
                            let reason = format!(
                                "artifact update could not be applied to the staged view for \
                                 {role:?}: {error}. Re-read the target file(s) and modify them \
                                 with file tools such as read_file, write_file, replace_text, or \
                                 delete_file before accepting."
                            );
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "DeliberationHandler",
                                format!("{role:?}"),
                                TelemetryEvent::StagedViewConstructionFailed {
                                    role: format!("{role:?}"),
                                    reason: reason.clone(),
                                },
                            ));
                            return DeliberationEvent::RoleReturned {
                                role,
                                result: super::event::RoleResult::Failed {
                                    kind: FailureKind::WorkSemanticValidationFailure,
                                    reason,
                                },
                            };
                        }
                    };

                if self.node_kind == NodeKind::Plan
                    && matches!(role, DeliberationRole::Producer)
                    && self.plan_validation_context.is_some()
                {
                    return self.run_plan_producer_with_validation(
                        ProducerSemanticValidationConfig {
                            role,
                            objective,
                            target_files,
                            producer_content,
                            critic_content,
                            initial_feedback: feedback,
                            max_retries: MAX_PLAN_VALIDATION_RETRIES,
                            accumulate_artifact_update_on_pass: false,
                        },
                        telemetry,
                    );
                }

                if self.node_kind == NodeKind::Work
                    && self.work_requires_artifact_update
                    && matches!(role, DeliberationRole::Producer)
                {
                    return self.run_work_producer_with_validation(
                        ProducerSemanticValidationConfig {
                            role,
                            objective,
                            target_files,
                            producer_content,
                            critic_content,
                            initial_feedback: feedback,
                            max_retries: MAX_WORK_SEMANTIC_VALIDATION_RETRIES,
                            accumulate_artifact_update_on_pass: true,
                        },
                        telemetry,
                    );
                }

                let request = RoleRequest {
                    role: role.clone(),
                    objective,
                    target_files,
                    test_plan_context: self.test_plan_context.clone(),
                    target_views,
                    producer_content,
                    critic_content,
                    feedback,
                    node_kind: self.node_kind.clone(),
                    tool_context,
                };
                let output = self.runner.run_role(request, telemetry);
                self.accumulate_artifact_update(output.artifact_update);
                DeliberationEvent::RoleReturned {
                    role,
                    result: output.result,
                }
            }
            DeliberationEffect::ReturnComplete { .. } => {
                unreachable!(
                    "ReturnComplete is a terminal effect; \
                     run_machine returns before dispatching it"
                )
            }
            DeliberationEffect::ReturnFailed { .. } => {
                unreachable!(
                    "ReturnFailed is a terminal effect; \
                     run_machine returns before dispatching it"
                )
            }
        }
    }

    /// Returns and clears the artifact update accumulated by tool loops across
    /// all role invocations in this handler. Returns `None` when no tool calls
    /// produced file changes.
    pub fn take_artifact_update(&self) -> Option<ArtifactUpdate> {
        let changes: Vec<FileChange> = self.accumulated_update.borrow_mut().drain(..).collect();
        if changes.is_empty() {
            None
        } else {
            Some(ArtifactUpdate { changes })
        }
    }
}
