use crate::machines::deliberation::event::RoleResult;
use crate::machines::scheduler::FailureKind;
use crate::node_runner::planner::parse_planner_content;
use crate::roles::runner::RoleRunner;
use crate::telemetry::TelemetrySink;

use super::event::DeliberationEvent;
use super::handler::DeliberationHandler;
use super::semantic_validation::{
    ProducerSemanticValidationConfig, ProducerSemanticValidationDecision, ValidationRetry,
};
use super::validation::{
    planner_parse_failure_feedback, planner_validation_feedback, validate_plan_output_for_context,
};

impl<R: RoleRunner> DeliberationHandler<R> {
    pub(crate) fn run_plan_producer_with_validation(
        &self,
        config: ProducerSemanticValidationConfig,
        telemetry: &dyn TelemetrySink,
    ) -> DeliberationEvent {
        let context = self
            .plan_validation_context
            .as_ref()
            .expect("plan_validation_context must be Some when this method is called");

        self.run_producer_semantic_validation_loop(
            config,
            telemetry,
            || None,
            |output| {
                let RoleResult::Accepted { content } = &output.result else {
                    return ProducerSemanticValidationDecision::Valid;
                };
                let Some(planner_out) = parse_planner_content(content) else {
                    return ProducerSemanticValidationDecision::Retry(ValidationRetry {
                        feedback_reason: planner_parse_failure_feedback(),
                        failure_kind: FailureKind::PlannerValidationFailure,
                        failure_reason:
                            "planner validation failed: content is not valid PlannerOutput JSON"
                                .to_string(),
                    });
                };
                match validate_plan_output_for_context(&planner_out, context) {
                    Ok(()) => ProducerSemanticValidationDecision::Valid,
                    Err(e) => ProducerSemanticValidationDecision::Retry(ValidationRetry {
                        feedback_reason: planner_validation_feedback(&e),
                        failure_kind: FailureKind::PlannerValidationFailure,
                        failure_reason: format!("planner validation failed: {e}"),
                    }),
                }
            },
        )
    }
}
