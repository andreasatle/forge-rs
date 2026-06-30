//! DeliberationMachine state types.
//!
//! The deliberation machine runs Producer → Critic → Referee before completing.
//! When the Referee rejects, the machine loops back to Producer with accumulated
//! feedback, up to `max_revisions` times. Final output is always the producer
//! content; critic and referee do not replace it.

use crate::machines::scheduler::FailureKind;

/// Feedback recorded when the Referee rejects a producer draft.
#[derive(Clone, Debug, PartialEq)]
pub struct RevisionFeedback {
    /// The reason the Referee gave for rejecting the draft.
    pub reason: String,
}

/// Durable state for producer semantic validation attempts.
#[derive(Clone, Debug, PartialEq)]
pub struct ProducerValidationState {
    /// Number of validation rejections that have already been retried.
    pub attempt: usize,
    /// Feedback from the most recent producer semantic validation rejection.
    pub feedback: Vec<RevisionFeedback>,
}

/// The input submitted to the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationRequest {
    /// The objective the pipeline should address.
    pub objective: String,
    /// Structured target files the pipeline should use for file-tool policy.
    pub target_files: Vec<String>,
    /// Maximum number of revision loops allowed.
    ///
    /// `0` means no revisions: the first Referee rejection fails immediately.
    pub max_revisions: usize,
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
        /// Human-readable description of why the pipeline failed.
        reason: String,
    },
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

/// The lifecycle of the deliberation machine.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationState {
    /// The machine has a request and is waiting for `Start`.
    Ready {
        /// The request that will be processed once the machine starts.
        request: DeliberationRequest,
    },

    /// A role has been dispatched; the machine is waiting for its result.
    Waiting {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// The role that was dispatched and has not yet responded.
        role: DeliberationRole,
        /// Content accepted by the Producer. `None` while waiting for Producer;
        /// `Some` while waiting for Critic or Referee.
        producer_content: Option<String>,
        /// Advisory result from the Critic stage. `None` until Critic completes.
        critic_advisory: Option<CriticAdvisory>,
        /// Feedback accumulated from each Referee rejection.
        /// The number of used revision loops is derived from this length.
        feedback: Vec<RevisionFeedback>,
        /// Producer semantic validation retry state. Empty until Producer
        /// output is being validated or retried.
        producer_validation: ProducerValidationState,
    },

    /// The pipeline finished successfully. Terminal state.
    Complete {
        /// The accepted output from the pipeline.
        output: DeliberationOutput,
    },

    /// The pipeline failed. Terminal state.
    Failed {
        /// Machine-readable failure cause.
        kind: FailureKind,
        /// Human-readable description of why the pipeline failed.
        reason: String,
    },
}
