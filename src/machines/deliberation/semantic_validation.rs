use crate::machines::deliberation::event::ProducerValidationResult;
use crate::machines::scheduler::NodeKind;
use crate::roles::runner::RoleRunner;

use super::handler::DeliberationHandler;

impl<R: RoleRunner> DeliberationHandler<R> {
    pub(crate) fn validate_producer_semantics(
        &self,
        content: &str,
        artifact_changed: bool,
    ) -> ProducerValidationResult {
        if self.node_kind == NodeKind::Plan && self.plan_validation_context.is_some() {
            return self.validate_plan_producer_content(content);
        }

        if self.node_kind == NodeKind::Work && self.work_requires_artifact_mutation {
            return self.validate_work_producer_output(artifact_changed);
        }

        ProducerValidationResult::Valid
    }
}
