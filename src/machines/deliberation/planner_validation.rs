use crate::machines::scheduler::FailureKind;
use crate::node_runner::planner::parse_planner_content;
use crate::roles::runner::RoleRunner;

use super::event::ProducerValidationResult;
use super::handler::DeliberationHandler;
use super::handler::MAX_PLAN_VALIDATION_RETRIES;
use super::validation::{
    planner_parse_failure_feedback, planner_validation_feedback, validate_plan_output_for_context,
};

impl<R: RoleRunner> DeliberationHandler<R> {
    pub(crate) fn validate_plan_producer_content(&self, content: &str) -> ProducerValidationResult {
        let context = self
            .plan_validation_context
            .as_ref()
            .expect("plan_validation_context must be Some when this method is called");

        let Some(planner_out) = parse_planner_content(content) else {
            return ProducerValidationResult::Retry {
                feedback_reason: planner_parse_failure_feedback(),
                max_retries: MAX_PLAN_VALIDATION_RETRIES,
                failure_kind: FailureKind::PlannerValidationFailure,
                failure_reason:
                    "planner validation failed: content is not valid PlannerOutput JSON".to_string(),
            };
        };

        match validate_plan_output_for_context(&planner_out, context) {
            Ok(()) => ProducerValidationResult::Valid,
            Err(e) => ProducerValidationResult::Retry {
                feedback_reason: planner_validation_feedback(&e),
                max_retries: MAX_PLAN_VALIDATION_RETRIES,
                failure_kind: FailureKind::PlannerValidationFailure,
                failure_reason: format!("planner validation failed: {e}"),
            },
        }
    }
}
