//! Events received by the deliberation machine.

use crate::machines::scheduler::FailureKind;

/// Retry metadata returned when accepted Producer output fails semantic validation.
#[derive(Clone, Debug, PartialEq)]
pub struct ProducerValidationRetry {
    /// Feedback to send to the next Producer attempt.
    pub feedback_reason: String,
    /// Maximum semantic validation retries allowed for this validation mode.
    pub max_retries: usize,
    /// Machine-readable terminal failure cause if retries are exhausted.
    pub failure_kind: FailureKind,
    /// Human-readable terminal failure reason if retries are exhausted.
    pub failure_reason: String,
}

/// Events that drive the deliberation machine forward.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationEvent {
    /// Bootstrap event — starts the pipeline from `Ready`.
    Start,
    /// A Producer role call accepted content and reported artifact mutation
    /// metadata needed by producer semantic validation.
    ProducerAccepted {
        /// The accepted Producer content.
        content: String,
        /// Whether the Producer role call changed the artifact workspace.
        artifact_changed: bool,
    },
    /// The Producer role completed successfully but rejected the task.
    ProducerRejected {
        /// Human-readable explanation of why the content was rejected.
        reason: String,
    },
    /// The Producer role could not execute.
    ProducerFailed {
        /// Machine-readable failure cause.
        kind: FailureKind,
        /// Human-readable description of the execution failure.
        reason: String,
    },
    /// Accepted Producer content passed semantic validation.
    ProducerValidationAccepted {
        /// The accepted Producer content that was validated.
        content: String,
    },
    /// Accepted Producer content failed semantic validation.
    ProducerValidationRejected {
        /// The accepted Producer content that was validated.
        content: String,
        /// Retry metadata and terminal failure details.
        retry: ProducerValidationRetry,
    },
    /// The Critic accepted and supplied review content for the Referee.
    CriticAccepted {
        /// The review content supplied by the Critic.
        content: String,
    },
    /// The Critic rejected and supplied an advisory reason for the Referee.
    CriticRejected {
        /// The rejection reason supplied by the Critic.
        reason: String,
    },
    /// The Critic role could not execute.
    CriticFailed {
        /// Machine-readable failure cause.
        kind: FailureKind,
        /// Human-readable description of the execution failure.
        reason: String,
    },
    /// The Referee accepted the Producer content.
    RefereeAccepted {
        /// The Referee content. The terminal output remains Producer content.
        content: String,
    },
    /// The Referee rejected the Producer content.
    RefereeRejected {
        /// Human-readable explanation of why the content was rejected.
        reason: String,
    },
    /// The Referee role could not execute.
    RefereeFailed {
        /// Machine-readable failure cause.
        kind: FailureKind,
        /// Human-readable description of the execution failure.
        reason: String,
    },
}
