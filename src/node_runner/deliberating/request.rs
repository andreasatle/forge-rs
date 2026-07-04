//! Mapping node-run inputs into deliberation state and handler wiring.

use std::sync::Arc;

use crate::machines::deliberation::PlanValidationContext;
use crate::machines::deliberation::{
    DeliberationRequest, DeliberationState, ProviderBackedDeliberationHandler,
};
use crate::machines::scheduler::NodeKind;
use crate::node_runner::TestTargetsFn;
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;

use crate::node_runner::types::NodeRunRequest;

use super::context::build_deliberation_context;

pub(crate) struct PreparedDeliberation<'a, P: ProviderClient> {
    pub(crate) initial_state: DeliberationState,
    pub(crate) handler: ProviderBackedDeliberationHandler<&'a P>,
}

pub(crate) fn prepare_deliberation<'a, P: ProviderClient>(
    provider: &'a P,
    request: &NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    required_test_targets_fn: &Arc<TestTargetsFn>,
    context_file_names: &[String],
) -> PreparedDeliberation<'a, P> {
    let plan_validation_context =
        build_plan_validation_context(request, Arc::clone(required_test_targets_fn));
    let context = build_deliberation_context(request, required_test_targets_fn, context_file_names);
    let initial_state = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: request.objective.clone(),
            context,
            node_kind: request.kind.clone(),
            worker_role: request.worker_role.clone(),
            test_plan_context: request.test_plan_context.clone(),
            max_revisions: 1,
        },
    };
    let handler = build_handler(
        provider,
        request,
        max_tokens,
        policy,
        plan_validation_context,
    );
    PreparedDeliberation {
        initial_state,
        handler,
    }
}

fn build_plan_validation_context(
    request: &NodeRunRequest,
    required_test_targets_fn: Arc<TestTargetsFn>,
) -> Option<PlanValidationContext> {
    if request.kind == NodeKind::Plan {
        Some(PlanValidationContext {
            required_test_targets_fn,
        })
    } else {
        None
    }
}

fn build_handler<'a, P: ProviderClient>(
    provider: &'a P,
    request: &NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    plan_validation_context: Option<PlanValidationContext>,
) -> ProviderBackedDeliberationHandler<&'a P> {
    if request.kind == NodeKind::Work && request.artifact_view.is_none() {
        ProviderBackedDeliberationHandler::new_non_artifact_work_with_policy(
            provider,
            max_tokens,
            policy.clone(),
        )
    } else {
        ProviderBackedDeliberationHandler::new_with_work_attempt(
            provider,
            request.artifact_view.clone(),
            max_tokens,
            request.kind.clone(),
            policy.clone(),
            plan_validation_context,
            request.work_attempt.clone(),
        )
    }
}
