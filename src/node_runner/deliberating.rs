//! NodeRunner backed by DeliberationMachine.

mod api_summary;
mod context;
mod execution;
mod machine;
mod output;
mod request;

use std::sync::Arc;

use crate::machines::scheduler::{ModelTier, NodeKind};
use crate::node_runner::{TestTargetsFn, ValidationPlanForRoleFn};
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;
use crate::telemetry::TelemetrySink;
use crate::validation::CommandSpec;

use super::runner::NodeRunner;
use super::types::{NodeRunRequest, NodeRunResult};

use self::context::DeliberationContextConfig;
use self::execution::run_with_provider;

/// Runs a node by driving a
/// [`DeliberationMachine`](crate::machines::deliberation::DeliberationMachine)
/// with a real provider.
///
/// Holds a separate provider and token budget for each [`ModelTier`]. On each
/// `run_node` call the runner inspects `request.model_tier` and routes to either
/// `cheap_provider` or `strong_provider`. When no strong provider is configured
/// the caller should pass the same provider for both tiers.
///
/// The final producer content is mapped to [`NodeRunResult`] by kind: plan nodes
/// produce child work nodes from structured planner output; work nodes return
/// the producer content as their summary. Artifact-backed Work mutates the
/// supplied [`WorkAttempt`](crate::node_runner::WorkAttempt) workspace directly.
///
/// When the request carries an [`ArtifactView`](crate::artifacts::ArtifactView),
/// a brief file listing (and `README.md` if present) is prepended to the
/// deliberation objective so the producer has file context without any workspace
/// mutation.
pub struct DeliberatingNodeRunner<C, S> {
    cheap_provider: C,
    strong_provider: S,
    cheap_max_tokens: u32,
    strong_max_tokens: u32,
    role_policy: RolePolicy,
    required_test_targets_fn: Arc<TestTargetsFn>,
    context_file_names: Vec<String>,
    api_summary_command: Option<CommandSpec>,
    /// Looks up the validation plan stamped onto a `Work` node request based
    /// on its assigned worker role, produced by this runner.
    ///
    /// Returning `None` for a role means no per-node plan; integration falls
    /// back to the global handler-level validator.
    validation_plan_for_role_fn: Arc<ValidationPlanForRoleFn>,
}

impl<C, S> DeliberatingNodeRunner<C, S> {
    /// Build a runner with separate cheap and strong providers.
    ///
    /// When no distinct strong provider is available, pass the same provider
    /// (or a reference to it) for both parameters — selection will still be
    /// explicit in the call site rather than accidental.
    pub fn new(cheap_provider: C, strong_provider: S) -> Self {
        Self {
            cheap_provider,
            strong_provider,
            cheap_max_tokens: 1024,
            strong_max_tokens: 1024,
            role_policy: RolePolicy::default(),
            required_test_targets_fn: Arc::new(|_| vec![]),
            context_file_names: vec![],
            api_summary_command: None,
            validation_plan_for_role_fn: Arc::new(|_| None),
        }
    }

    /// Set the token budget forwarded to cheap-tier role calls.
    pub fn with_cheap_max_tokens(mut self, max_tokens: u32) -> Self {
        self.cheap_max_tokens = max_tokens;
        self
    }

    /// Set the token budget forwarded to strong-tier role calls.
    pub fn with_strong_max_tokens(mut self, max_tokens: u32) -> Self {
        self.strong_max_tokens = max_tokens;
        self
    }

    /// Override the role prompt policy supplied to each role invocation.
    ///
    /// The policy is cloned once per node run and forwarded to the deliberation
    /// handler. The default is [`RolePolicy::default()`], which preserves the
    /// hardcoded behaviour.
    pub fn with_role_policy(mut self, policy: RolePolicy) -> Self {
        self.role_policy = policy;
        self
    }

    /// Supply the project adapter's test-target derivation function.
    ///
    /// The function receives the source targets in the plan and returns the
    /// test-file paths the adapter requires. An empty return means no tests
    /// are required for a given set of targets.
    pub fn with_required_test_targets_fn(mut self, f: Arc<TestTargetsFn>) -> Self {
        self.required_test_targets_fn = f;
        self
    }

    /// Set the artifact file names whose contents are prepended as ambient
    /// context in the deliberation objective. Provided by the project adapter.
    pub fn with_context_file_names(mut self, names: Vec<String>) -> Self {
        self.context_file_names = names;
        self
    }

    /// Supply the language plugin's `api_summary` command, run per file
    /// against `Decomposition` and `Plan` node artifacts to surface existing
    /// API shape to the planner. Absent by default, which omits the section.
    pub fn with_api_summary_command(mut self, command: Option<CommandSpec>) -> Self {
        self.api_summary_command = command;
        self
    }

    /// Supply the per-role validation plan lookup stamped onto every `Work`
    /// node this runner produces.
    ///
    /// Called with each node's assigned worker role when a plan node
    /// expands, and the resulting plan is stamped onto that
    /// [`NodeRequest`](crate::machines::scheduler::NodeRequest). When not set
    /// (the default), nodes carry no plan and integration falls back to the
    /// handler-level validator.
    pub fn with_validation_plan_for_role_fn(mut self, f: Arc<ValidationPlanForRoleFn>) -> Self {
        self.validation_plan_for_role_fn = f;
        self
    }
}

impl<C: ProviderClient, S: ProviderClient> NodeRunner for DeliberatingNodeRunner<C, S> {
    fn run_node(&self, request: NodeRunRequest, telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let context_config = DeliberationContextConfig {
            required_test_targets_fn: &self.required_test_targets_fn,
            context_file_names: &self.context_file_names,
            api_summary_command: self.api_summary_command.as_ref(),
        };
        let result = match request.model_tier {
            ModelTier::Cheap => run_with_provider(
                &self.cheap_provider,
                request,
                self.cheap_max_tokens,
                &self.role_policy,
                &context_config,
                telemetry,
            ),
            ModelTier::Strong => run_with_provider(
                &self.strong_provider,
                request,
                self.strong_max_tokens,
                &self.role_policy,
                &context_config,
                telemetry,
            ),
        };

        if let NodeRunResult::PlanAccepted(plan) = result {
            NodeRunResult::PlanAccepted(self.stamp_plan_metadata(plan))
        } else {
            result
        }
    }
}

impl<C, S> DeliberatingNodeRunner<C, S> {
    /// Stamp the plan-derived metadata and the correct validation plan onto
    /// every `Work` [`NodeRequest`] in `plan`, based on each child's worker
    /// role.
    ///
    /// A `Decomposition` parent's children are themselves `Decomposition` or
    /// `Plan` nodes, so this is a no-op for them — they carry no worker role
    /// or concrete targets yet. A `Plan` parent's children are `Work` nodes
    /// and get their validation plan stamped here.
    fn stamp_plan_metadata(
        &self,
        mut plan: crate::machines::scheduler::PlanOutput,
    ) -> crate::machines::scheduler::PlanOutput {
        for child in &mut plan.children {
            if child.kind != NodeKind::Work {
                continue;
            }
            child.required_validation_targets =
                (self.required_test_targets_fn)(&child.target_files);
            child.validation_plan =
                (self.validation_plan_for_role_fn)(child.worker_role.as_deref());
        }
        plan
    }
}

#[cfg(test)]
mod tests;
