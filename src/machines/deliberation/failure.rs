//! Deliberation failure reason type.

use super::types::DeliberationRole;

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
