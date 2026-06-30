//! Shared vocabulary types for the deliberation pipeline.

use crate::machines::scheduler::FailureKind;

use super::failure::DeliberationFailureReason;

/// Feedback recorded when the Referee rejects a producer draft.
#[derive(Clone, Debug, PartialEq)]
pub struct RevisionFeedback {
    /// The reason the Referee gave for rejecting the draft.
    pub reason: String,
}

/// Advisory output from the Critic stage, preserved with its semantic outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum CriticAdvisory {
    /// The Critic accepted and supplied review content for the Referee.
    AcceptedReview {
        /// The review content supplied by the Critic.
        content: String,
    },
    /// The Critic rejected and supplied a reason for the Referee to consider.
    RejectedReason {
        /// The rejection reason supplied by the Critic.
        reason: String,
    },
}

impl CriticAdvisory {
    /// Text form passed to the existing role prompt boundary.
    pub fn as_referee_content(&self) -> &str {
        match self {
            CriticAdvisory::AcceptedReview { content } => content,
            CriticAdvisory::RejectedReason { reason } => reason,
        }
    }
}

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
