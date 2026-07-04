//! NodeRunner backed by DeliberationMachine.

mod context;
mod execution;
mod machine;
mod output;
mod request;

use std::sync::Arc;

use crate::machines::scheduler::{ModelTier, NodeKind};
use crate::node_runner::TestTargetsFn;
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;
use crate::telemetry::TelemetrySink;
use crate::validation::ValidationPlan;

use super::runner::NodeRunner;
use super::types::{NodeRunRequest, NodeRunResult};

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
    /// Validation plan stamped onto every non-tester `Work` node request
    /// produced by this runner.
    ///
    /// `None` means no per-node plan; integration falls back to the global
    /// handler-level validator.
    work_node_plan: Option<ValidationPlan>,
    /// Validation plan stamped onto every tester-role `Work` node request
    /// produced by this runner.
    ///
    /// `None` means no per-node plan; integration falls back to the global
    /// handler-level validator.
    validation_node_plan: Option<ValidationPlan>,
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
            work_node_plan: None,
            validation_node_plan: None,
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

    /// Supply the validation plan stamped onto every `Work` node this runner
    /// produces.
    ///
    /// The plan is cloned onto each [`NodeRequest`](crate::machines::scheduler::NodeRequest)
    /// when a plan node expands.  When not set (the default), nodes carry no
    /// plan and integration falls back to the handler-level validator.
    pub fn with_work_node_plan(mut self, plan: Option<ValidationPlan>) -> Self {
        self.work_node_plan = plan;
        self
    }

    /// Supply the validation plan stamped onto every tester-role `Work` node
    /// this runner produces.
    ///
    /// The plan is cloned onto each [`NodeRequest`](crate::machines::scheduler::NodeRequest)
    /// when a plan node expands.  When not set (the default), nodes carry no
    /// plan and integration falls back to the handler-level validator.
    pub fn with_validation_node_plan(mut self, plan: Option<ValidationPlan>) -> Self {
        self.validation_node_plan = plan;
        self
    }
}

impl<C: ProviderClient, S: ProviderClient> NodeRunner for DeliberatingNodeRunner<C, S> {
    fn run_node(&self, request: NodeRunRequest, telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let result = match request.model_tier {
            ModelTier::Cheap => run_with_provider(
                &self.cheap_provider,
                request,
                self.cheap_max_tokens,
                &self.role_policy,
                &self.required_test_targets_fn,
                &self.context_file_names,
                telemetry,
            ),
            ModelTier::Strong => run_with_provider(
                &self.strong_provider,
                request,
                self.strong_max_tokens,
                &self.role_policy,
                &self.required_test_targets_fn,
                &self.context_file_names,
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
    /// every non-`Plan` [`NodeRequest`] in `plan`, based on each child's
    /// worker role.
    fn stamp_plan_metadata(
        &self,
        mut plan: crate::machines::scheduler::PlanOutput,
    ) -> crate::machines::scheduler::PlanOutput {
        for child in &mut plan.children {
            if child.kind != NodeKind::Work {
                continue;
            }
            if child.worker_role.as_deref() == Some("tester") {
                if self.validation_node_plan.is_some() {
                    child.validation_plan = self.validation_node_plan.clone();
                }
            } else {
                child.required_validation_targets =
                    (self.required_test_targets_fn)(&child.target_files);
                if self.work_node_plan.is_some() {
                    child.validation_plan = self.work_node_plan.clone();
                }
            }
        }
        plan
    }
}

#[cfg(test)]
mod tests;
