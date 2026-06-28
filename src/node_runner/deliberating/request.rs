//! Mapping node-run inputs into deliberation state and handler wiring.

use crate::machines::deliberation::{
    DeliberationRequest, DeliberationState, ProviderBackedDeliberationHandler,
};
use crate::machines::scheduler::NodeKind;
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;

use crate::node_runner::types::NodeRunRequest;

use super::context::enrich_objective;

pub(crate) struct PreparedDeliberation<'a, P: ProviderClient> {
    pub(crate) initial_state: DeliberationState,
    pub(crate) handler: ProviderBackedDeliberationHandler<&'a P>,
}

pub(crate) fn prepare_deliberation<'a, P: ProviderClient>(
    provider: &'a P,
    request: &NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    requires_tests: bool,
    context_file_names: &[String],
) -> PreparedDeliberation<'a, P> {
    let plan_validation_context = build_plan_validation_context(request, requires_tests);
    let objective = enrich_objective(request, requires_tests, context_file_names);
    let initial_state = DeliberationState::Ready {
        request: DeliberationRequest {
            objective,
            target_files: request.target_files.clone(),
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
    requires_tests: bool,
) -> Option<(String, Vec<String>, bool)> {
    let top_objective = request.objective.clone();
    let existing_files: Vec<String> = request
        .artifact_view
        .as_ref()
        .and_then(|v| v.list_files().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if request.kind == NodeKind::Plan {
        Some((top_objective, existing_files, requires_tests))
    } else if existing_files.is_empty() && !requires_tests {
        None
    } else {
        Some((top_objective, existing_files, requires_tests))
    }
}

fn build_handler<'a, P: ProviderClient>(
    provider: &'a P,
    request: &NodeRunRequest,
    max_tokens: u32,
    policy: &RolePolicy,
    plan_validation_context: Option<(String, Vec<String>, bool)>,
) -> ProviderBackedDeliberationHandler<&'a P> {
    if request.kind == NodeKind::Work && request.artifact_view.is_none() {
        ProviderBackedDeliberationHandler::new_non_artifact_work_with_policy(
            provider,
            max_tokens,
            policy.clone(),
        )
    } else {
        ProviderBackedDeliberationHandler::new_with_view(
            provider,
            request.artifact_view.clone(),
            max_tokens,
            request.kind.clone(),
            policy.clone(),
            plan_validation_context,
        )
    }
}
