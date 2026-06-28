//! NodeRunner backed by DeliberationMachine.

mod context;
mod execution;
mod machine;
mod output;
mod request;

use crate::machines::scheduler::{ModelTier, NodeKind};
use crate::providers::ProviderClient;
use crate::roles::RolePolicy;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::planner::try_fast_plan;
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
/// produce one child work node whose objective is the producer content; work nodes
/// return the producer content as their summary and write it to `output.txt` in an
/// [`ArtifactUpdate`](crate::artifacts::ArtifactUpdate). No JSON interpretation
/// happens here — that boundary belongs to the deliberation role handler.
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
    requires_tests: bool,
    context_file_names: Vec<String>,
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
            requires_tests: false,
            context_file_names: vec![],
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

    /// Require planner output for code changes to include test-related targets.
    pub fn with_requires_tests(mut self, requires_tests: bool) -> Self {
        self.requires_tests = requires_tests;
        self
    }

    /// Set the artifact file names whose contents are prepended as ambient
    /// context in the deliberation objective. Provided by the project adapter.
    pub fn with_context_file_names(mut self, names: Vec<String>) -> Self {
        self.context_file_names = names;
        self
    }
}

impl<C: ProviderClient, S: ProviderClient> NodeRunner for DeliberatingNodeRunner<C, S> {
    fn run_node(&self, request: NodeRunRequest, telemetry: &dyn TelemetrySink) -> NodeRunResult {
        // Fast path: bypass LLM for plan nodes whose objective names exactly one source file.
        if request.kind == NodeKind::Plan
            && let Some(plan) = try_fast_plan(&request.objective, self.requires_tests)
        {
            let task_count = plan.children.len();
            telemetry.record(TelemetryRecord::new(
                "DeliberatingNodeRunner",
                TelemetryEvent::FastPlanUsed { task_count },
            ));
            return NodeRunResult::PlanAccepted(plan);
        }

        match request.model_tier {
            ModelTier::Cheap => run_with_provider(
                &self.cheap_provider,
                request,
                self.cheap_max_tokens,
                &self.role_policy,
                self.requires_tests,
                &self.context_file_names,
                telemetry,
            ),
            ModelTier::Strong => run_with_provider(
                &self.strong_provider,
                request,
                self.strong_max_tokens,
                &self.role_policy,
                self.requires_tests,
                &self.context_file_names,
                telemetry,
            ),
        }
    }
}

#[cfg(test)]
mod tests;
