//! Effects emitted by the deliberation machine.
//!
//! Effects are commands. The machine emits them; the handler executes them and
//! converts the external result back into a `DeliberationEvent`.

use super::request::DeliberationContext;
use super::state::{DeliberationRole, RevisionFeedback};

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
        context: DeliberationContext,
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
    },
}
