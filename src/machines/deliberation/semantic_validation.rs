use crate::artifacts::ArtifactUpdate;
use crate::machines::deliberation::event::RoleResult;
use crate::machines::deliberation::state::{DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{FailureKind, NodeKind};
use crate::roles::TargetView;
use crate::roles::runner::{RoleRequest, RoleRunOutput, RoleRunner, RoleToolContext};
use crate::telemetry::TelemetrySink;

use super::event::DeliberationEvent;
use super::handler::{DeliberationHandler, TARGET_VIEW_BUDGET};

pub(crate) struct ProducerSemanticValidationConfig {
    pub(crate) role: DeliberationRole,
    pub(crate) objective: String,
    pub(crate) target_files: Vec<String>,
    pub(crate) producer_content: Option<String>,
    pub(crate) critic_content: Option<String>,
    pub(crate) initial_feedback: Vec<RevisionFeedback>,
    pub(crate) max_retries: usize,
    pub(crate) accumulate_artifact_update_on_pass: bool,
}

pub(crate) enum ProducerSemanticValidationDecision {
    Valid,
    Retry(ValidationRetry),
}

pub(crate) struct ValidationRetry {
    pub(crate) feedback_reason: String,
    pub(crate) failure_kind: FailureKind,
    pub(crate) failure_reason: String,
}

impl<R: RoleRunner> DeliberationHandler<R> {
    pub(crate) fn run_producer_semantic_validation_loop(
        &self,
        config: ProducerSemanticValidationConfig,
        telemetry: &dyn TelemetrySink,
        mut tool_context_for_attempt: impl FnMut() -> Option<RoleToolContext>,
        mut validate: impl FnMut(&RoleRunOutput) -> ProducerSemanticValidationDecision,
    ) -> DeliberationEvent {
        let mut feedback = config.initial_feedback;
        let base_target_views: Vec<TargetView> = if self.node_kind == NodeKind::Work {
            self.artifact_view
                .as_ref()
                .map(|base| {
                    crate::project::build_file_text_target_views(
                        base,
                        &config.target_files,
                        TARGET_VIEW_BUDGET,
                    )
                })
                .unwrap_or_default()
        } else {
            vec![]
        };

        for attempt in 0..=config.max_retries {
            let request = RoleRequest {
                role: config.role.clone(),
                objective: config.objective.clone(),
                target_files: config.target_files.clone(),
                test_plan_context: self.test_plan_context.clone(),
                target_views: base_target_views.clone(),
                producer_content: config.producer_content.clone(),
                critic_content: config.critic_content.clone(),
                feedback: feedback.clone(),
                node_kind: self.node_kind.clone(),
                tool_context: tool_context_for_attempt(),
            };
            let output = self.runner.run_role(request, telemetry);

            if !matches!(output.result, RoleResult::Accepted { .. }) {
                return DeliberationEvent::RoleReturned {
                    role: config.role,
                    result: output.result,
                };
            }

            match validate(&output) {
                ProducerSemanticValidationDecision::Valid => {
                    if config.accumulate_artifact_update_on_pass {
                        self.accumulate_artifact_update(output.artifact_update);
                    }
                    return DeliberationEvent::RoleReturned {
                        role: config.role,
                        result: output.result,
                    };
                }
                ProducerSemanticValidationDecision::Retry(retry) => {
                    if attempt >= config.max_retries {
                        return DeliberationEvent::RoleReturned {
                            role: config.role,
                            result: RoleResult::Failed {
                                kind: retry.failure_kind,
                                reason: retry.failure_reason,
                            },
                        };
                    }
                    feedback = vec![RevisionFeedback {
                        reason: retry.feedback_reason,
                    }];
                }
            }
        }

        unreachable!("loop exits via return in all branches")
    }

    pub(crate) fn accumulate_artifact_update(&self, update: Option<ArtifactUpdate>) {
        if let Some(update) = update {
            self.accumulated_update.borrow_mut().extend(update.changes);
        }
    }
}
