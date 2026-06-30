//! Shared vocabulary types for the deliberation pipeline.

use crate::machines::scheduler::FailureKind;

use super::failure::DeliberationFailureReason;

/// The three roles that participate in the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationRole {
    /// Generates the initial content for the objective.
    Producer,
    /// Evaluates the producer's content and accepts or rejects it.
    Critic,
    /// Makes the final acceptance decision after the critic has weighed in.
    Referee,
}

/// The final output produced by the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationOutput {
    /// The accepted producer content.
    pub content: String,
}

/// Terminal result returned by `run_machine` for the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationTerminalOutput {
    /// The pipeline completed successfully.
    Complete(DeliberationOutput),
    /// The pipeline failed before producing accepted content.
    Failed {
        /// Machine-readable failure cause.
        kind: FailureKind,
        /// Machine-readable terminal failure reason.
        reason: DeliberationFailureReason,
        /// Human-readable diagnostic text.
        message: String,
    },
}
