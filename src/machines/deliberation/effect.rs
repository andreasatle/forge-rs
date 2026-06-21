//! Effects emitted by the deliberation machine.
//!
//! Effects are commands. The machine emits them; the handler executes them and
//! converts the external result back into a `DeliberationEvent`.

use super::state::{DeliberationOutput, DeliberationRole};

/// Commands emitted by the deliberation machine.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationEffect {
    /// Dispatch the given role with the supplied objective.
    RunRole {
        /// The role to invoke.
        role: DeliberationRole,
        /// The objective to pass to the role.
        objective: String,
        /// Prior-stage content to pass to the role. `None` for Producer;
        /// `Some(producer_content)` for Critic.
        input: Option<String>,
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
