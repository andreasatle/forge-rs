use crate::machines::deliberation::state::DeliberationRole;
use crate::machines::scheduler::FailureKind;
use crate::roles::runner::{RoleRequest, RoleRunner};
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;
use super::handler::DeliberationHandler;

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
                                "WorkAttempt workspace view could not be constructed for \
                                 {role:?}: {error}. Use write_file by default when creating a \
                                 file or replacing most or all of an existing file. Use \
                                 replace_text only for small, localized edits after reading the \
                                 file and providing an exact old string that occurs once; \
                                 whitespace, indentation, or formatting differences will cause \
                                 replace_text to fail."
                            );
                            telemetry.record(TelemetryRecord::new_with_subsource(
                                "DeliberationHandler",
                                format!("{role:?}"),
                                TelemetryEvent::WorkAttemptViewConstructionFailed {
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
                match (&role, output.result) {
                    (
                        DeliberationRole::Producer,
                        super::event::RoleResult::Accepted { content },
                    ) => DeliberationEvent::ProducerAccepted {
                        content,
                        artifact_changed: output.artifact_changed,
                    },
                    (_, result) => DeliberationEvent::RoleReturned { role, result },
                }
            }
            DeliberationEffect::ValidateProducer {
                content,
                artifact_changed,
            } => {
                let result = self.validate_producer_semantics(&content, artifact_changed);
                DeliberationEvent::ProducerValidationReturned { content, result }
            }
        }
    }
}
