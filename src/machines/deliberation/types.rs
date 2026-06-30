//! Shared vocabulary types for the deliberation pipeline.

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

/// Prompt context that travels beside the canonical objective.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeliberationContext {
    /// Structured target files the pipeline should use for file-tool policy
    /// and prompt context.
    pub target_files: Vec<String>,
    /// Adapter-level testing requirement for code changes, when present.
    pub testing_requirement: Option<String>,
    /// Read-only artifact context made visible to roles.
    pub artifact: Option<ArtifactContext>,
}

/// Read-only artifact context captured before the deliberation run.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactContext {
    /// Existing files in the artifact.
    pub files: Vec<String>,
    /// Selected file contents included as prompt context.
    pub selected_files: Vec<SelectedFileContent>,
}

/// Content for a selected artifact file.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedFileContent {
    /// Artifact-relative path.
    pub path: String,
    /// File content at the captured artifact commit.
    pub content: String,
}

/// The input submitted to the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationRequest {
    /// The canonical objective the pipeline should address.
    pub objective: String,
    /// Structured prompt/tooling context for this run.
    pub context: DeliberationContext,
    /// Maximum number of revision loops allowed.
    ///
    /// `0` means no revisions: the first Referee rejection fails immediately.
    pub max_revisions: usize,
}

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
