use crate::machines::scheduler::FailureKind;
use crate::roles::runner::{RoleRunner, RoleToolContext};
use crate::telemetry::TelemetrySink;

use super::event::DeliberationEvent;
use super::handler::DeliberationHandler;
use super::semantic_validation::{
    ProducerSemanticValidationConfig, ProducerSemanticValidationDecision, ValidationRetry,
};
use super::validation::{validate_work_output, work_validation_feedback};

impl<R: RoleRunner> DeliberationHandler<R> {
    pub(crate) fn run_work_producer_with_validation(
        &self,
        config: ProducerSemanticValidationConfig,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        self.run_producer_semantic_validation_loop(
            config,
            telemetry,
            || {
                self.artifact_view.as_ref().map(|base| RoleToolContext {
                    artifact_view: Box::new(base.clone()),
                })
            },
            |output| match validate_work_output(output.artifact_update.as_ref()) {
                Ok(()) => ProducerSemanticValidationDecision::Valid,
                Err(e) => ProducerSemanticValidationDecision::Retry(ValidationRetry {
                    feedback_reason: work_validation_feedback(&e),
                    failure_kind: FailureKind::WorkSemanticValidationFailure,
                    failure_reason: format!("work semantic validation failed: {e}"),
                }),
            },
        )
    }
}
