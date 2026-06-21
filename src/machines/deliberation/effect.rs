//! Effects emitted by the deliberation machine.
//!
//! Effects are commands. The machine emits them; the handler executes them and
//! converts the external result back into a `DeliberationEvent`.

use super::state::{DeliberationOutput, DeliberationRole};

/// Commands emitted by the deliberation machine.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationEffect {
    /// Dispatch the given role with the supplied objective and prior-stage content.
    ///
    /// Semantics by role:
    /// - Producer: both fields are `None`.
    /// - Critic: `producer_content` is `Some`; `critic_content` is `None`.
    /// - Referee: both fields are `Some`.
    RunRole {
        /// The role to invoke.
        role: DeliberationRole,
        /// The objective to pass to the role.
        objective: String,
        /// Content produced by the Producer. `None` when dispatching Producer.
        producer_content: Option<String>,
        /// Content produced by the Critic. `None` when dispatching Producer or Critic.
        critic_content: Option<String>,
    },
    /// Signal successful completion to the caller.
    ReturnComplete {
        /// The accepted output to return to the caller.
        output: DeliberationOutput,
    },
    /// Signal failure to the caller.
    ReturnFailed {
        /// Human-readable description of the failure.
        reason: String,
    },
}
