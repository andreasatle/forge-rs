use crate::machines::scheduler::FailureKind;
use crate::roles::runner::RoleRunner;

use super::event::ProducerValidationResult;
use super::handler::DeliberationHandler;
use super::handler::MAX_WORK_SEMANTIC_VALIDATION_RETRIES;
use super::validation::{validate_work_output, work_validation_feedback};

impl<R: RoleRunner> DeliberationHandler<R> {
    pub(crate) fn validate_work_producer_output(
        &self,
        artifact_changed: bool,
    ) -> ProducerValidationResult {
        match validate_work_output(artifact_changed) {
            Ok(()) => ProducerValidationResult::Valid,
            Err(e) => ProducerValidationResult::Retry {
                feedback_reason: work_validation_feedback(&e),
                max_retries: MAX_WORK_SEMANTIC_VALIDATION_RETRIES,
                failure_kind: FailureKind::WorkSemanticValidationFailure,
                failure_reason: format!("work semantic validation failed: {e}"),
            },
        }
    }
}
