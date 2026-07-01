//! Effects emitted by the deliberation machine.
//!
//! Effects are commands. The machine emits them; the handler executes them and
//! converts the external result back into a `DeliberationEvent`.

use super::types::DeliberationContext;
use super::types::{DeliberationRole, RevisionFeedback};
use crate::machines::scheduler::{NodeKind, TestPlanContext};

/// Commands emitted by the deliberation machine.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationEffect {
    /// Dispatch the given role with the supplied objective and prior-stage content.
    ///
    /// Semantics by role:
    /// - Producer: `producer_content` and `critic_content` are `None`;
    ///   `feedback` carries accumulated Referee rejections (empty on the first pass).
    /// - Critic: `producer_content` is `Some`; `critic_content` is `None`.
    /// - Referee: both `producer_content` and `critic_content` are `Some`.
    RunRole {
        /// The role to invoke.
        role: DeliberationRole,
        /// The objective to pass to the role.
        objective: String,
        /// Structured prompt/tooling context for this run.
        context: Box<DeliberationContext>,
        /// Whether this run is for a plan node or a work node. Selects the
        /// matching node-kind-specific system prompt from the role policy.
        node_kind: NodeKind,
        /// Structured test-target planning context forwarded to the role prompt.
        test_plan_context: TestPlanContext,
        /// Content produced by the Producer. `None` when dispatching Producer.
        producer_content: Option<String>,
        /// Content produced by the Critic. `None` when dispatching Producer or Critic.
        critic_content: Option<String>,
        /// Accumulated Referee rejection feedback. Empty on the first pass.
        feedback: Vec<RevisionFeedback>,
    },
    /// Validate accepted Producer output before Critic sees it.
    ValidateProducer {
        /// The content accepted by the Producer role.
        content: String,
        /// Whether the Producer role mutated the artifact workspace.
        artifact_changed: bool,
        /// Whether this run is for a plan node or a work node. Selects
        /// plan-shaped vs work-shaped semantic validation.
        node_kind: NodeKind,
    },
}
