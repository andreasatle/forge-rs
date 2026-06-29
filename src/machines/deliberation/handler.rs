//! Effect handler for `DeliberationMachine`.
//!
//! `DeliberationHandler` is a thin adapter: it unpacks a `RunRole` effect,
//! delegates to a [`RoleRunner`](crate::roles::runner::RoleRunner), and wraps the result back into a
//! `RoleReturned` event. All prompt rendering, provider calls, JSON parsing,
//! protocol retries, and file tool loops live in the runner layer.

use crate::artifacts::ArtifactView;
use crate::machines::scheduler::{NodeKind, TestPlanContext};
use crate::node_runner::WorkAttempt;
use crate::roles::policy::RolePolicy;
use crate::roles::runner::ProviderRoleRunner;

use super::validation::PlanValidationContext;

/// Maximum retry attempts after the first accepted plan violates structured
/// planner validation.
pub(crate) const MAX_PLAN_VALIDATION_RETRIES: usize = 2;

/// Maximum retry attempts after the first accepted work result contains no
/// artifact file changes.
pub(crate) const MAX_WORK_SEMANTIC_VALIDATION_RETRIES: usize = 2;

/// Maximum bytes per target file to include in the prompt target-state view.
pub(crate) const TARGET_VIEW_BUDGET: usize = 16 * 1024;

/// Executes `DeliberationEffect` values by delegating role execution to a
/// [`RoleRunner`](crate::roles::runner::RoleRunner).
///
pub struct DeliberationHandler<R> {
    pub(crate) runner: R,
    /// Artifact view made available to roles as file tool context.
    pub(crate) artifact_view: Option<ArtifactView>,
    /// Live candidate workspace for artifact-producing Work.
    pub(crate) work_attempt: Option<WorkAttempt>,
    /// Whether this deliberation is for a plan node or a work node.
    /// Forwarded to every Producer RoleRequest to select the correct policy field.
    pub(crate) node_kind: NodeKind,
    /// Whether Work+Producer accepted output must mutate the artifact workspace.
    pub(crate) work_requires_artifact_mutation: bool,
    /// Structured test-target planning context forwarded to role prompts.
    pub(crate) test_plan_context: TestPlanContext,
    /// For plan nodes: optional structured validation applied to planner
    /// output before the plan is accepted.
    pub(crate) plan_validation_context: Option<PlanValidationContext>,
}

/// Compatibility alias: a [`DeliberationHandler`] backed by a
/// [`ProviderRoleRunner`].
pub type ProviderBackedDeliberationHandler<P> = DeliberationHandler<ProviderRoleRunner<P>>;

impl<P> DeliberationHandler<ProviderRoleRunner<P>> {
    /// Wrap a provider for explicit non-artifact Work.
    ///
    /// This is intended for demos/tests that want Producer/Critic/Referee
    /// deliberation without file tools. Accepted Work from this handler is a
    /// summary only and does not run artifact semantic validation.
    pub fn new_non_artifact_work(provider: P) -> Self {
        Self {
            runner: ProviderRoleRunner::new(provider),
            artifact_view: None,
            work_attempt: None,
            node_kind: NodeKind::Work,
            work_requires_artifact_mutation: false,
            test_plan_context: TestPlanContext::default(),
            plan_validation_context: None,
        }
    }

    /// Wrap a provider for explicit non-artifact Work with runner options.
    pub fn new_non_artifact_work_with_policy(
        provider: P,
        max_tokens: u32,
        policy: RolePolicy,
    ) -> Self {
        Self {
            runner: ProviderRoleRunner::new_with_max_tokens(provider, max_tokens)
                .with_policy(policy),
            artifact_view: None,
            work_attempt: None,
            node_kind: NodeKind::Work,
            work_requires_artifact_mutation: false,
            test_plan_context: TestPlanContext::default(),
            plan_validation_context: None,
        }
    }

    /// Wrap a provider in a handler with an artifact view for Work nodes, an
    /// explicit token budget forwarded to the role runner, the node kind
    /// used to select the matching plan/work system prompt from the policy,
    /// the role policy to inject into the runner, and an optional context used
    /// to reject planner tasks that violate structured plan rules.
    #[cfg(test)]
    pub(crate) fn new_with_view(
        provider: P,
        artifact_view: Option<ArtifactView>,
        max_tokens: u32,
        node_kind: NodeKind,
        policy: RolePolicy,
        plan_validation_context: Option<PlanValidationContext>,
        test_plan_context: TestPlanContext,
    ) -> Self {
        Self::new_with_work_attempt(
            provider,
            artifact_view,
            max_tokens,
            node_kind,
            policy,
            plan_validation_context,
            test_plan_context,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_work_attempt(
        provider: P,
        artifact_view: Option<ArtifactView>,
        max_tokens: u32,
        node_kind: NodeKind,
        policy: RolePolicy,
        plan_validation_context: Option<PlanValidationContext>,
        test_plan_context: TestPlanContext,
        work_attempt: Option<WorkAttempt>,
    ) -> Self {
        assert!(
            node_kind != NodeKind::Work || artifact_view.is_some(),
            "artifact-producing Work handlers require an ArtifactView; use \
             new_non_artifact_work for explicit summary-only Work"
        );
        Self {
            runner: ProviderRoleRunner::new_with_max_tokens(provider, max_tokens)
                .with_policy(policy),
            artifact_view,
            work_attempt,
            work_requires_artifact_mutation: node_kind == NodeKind::Work,
            test_plan_context,
            node_kind,
            plan_validation_context,
        }
    }
}

#[cfg(test)]
#[path = "handler_tests.rs"]
mod handler_tests;
