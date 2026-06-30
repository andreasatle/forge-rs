//! DeliberationMachine state types.
//!
//! The deliberation machine runs Producer → Critic → Referee before completing.
//! When the Referee rejects, the machine loops back to Producer with accumulated
//! feedback, up to `max_revisions` times. Final output is always the producer
//! content; critic and referee do not replace it.

use crate::machines::scheduler::FailureKind;

use super::request::DeliberationRequest;

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

/// The final output produced by the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationOutput {
    /// The accepted producer content.
    pub content: String,
}

/// Machine-readable terminal failure cause for the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationFailureReason {
    /// A role returned successfully but the Producer rejected the task.
    ProducerRejected,
    /// A role returned an execution failure.
    RoleFailed {
        /// The role whose execution failed.
        role: DeliberationRole,
    },
    /// Producer semantic validation exhausted its retry budget.
    ProducerValidationRetriesExhausted,
    /// Referee rejection exhausted the revision budget.
    RevisionLimitExhausted,
    /// The machine received an event that violates the expected role protocol.
    ProtocolViolation,
    /// The state/event pair is not a valid transition.
    InvalidTransition,
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

    /// The Producer has been dispatched; the machine is waiting for its result.
    WaitingProducer {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
        /// Producer semantic validation retry state.
        producer_validation: ProducerValidationState,
    },

    /// The Producer has accepted; the machine is waiting for semantic validation.
    ValidatingProducer {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// The content accepted by the Producer, under validation.
        producer_content: String,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
        /// Producer semantic validation retry state.
        producer_validation: ProducerValidationState,
    },

    /// The Critic has been dispatched; the machine is waiting for its result.
    WaitingCritic {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// Content accepted by the Producer (always present at this stage).
        producer_content: String,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
    },

    /// The Referee has been dispatched; the machine is waiting for its result.
    WaitingReferee {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// Content accepted by the Producer (always present at this stage).
        producer_content: String,
        /// Advisory result from the Critic stage (always present at this stage).
        critic_advisory: CriticAdvisory,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
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
        /// Machine-readable terminal failure reason.
        reason: DeliberationFailureReason,
        /// Human-readable diagnostic text.
        message: String,
    },
}
