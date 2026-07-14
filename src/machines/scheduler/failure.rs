//! Scheduler failure types.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Machine-readable cause of a node or integration failure.
///
/// `message` fields on failure payloads remain human-readable diagnostics only;
/// recovery policy must switch on this kind instead of parsing message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Provider transport, timeout, rate-limit, or other retryable provider failure.
    ProviderFailure,
    /// Provider failure known not to benefit from retrying, such as bad auth/config.
    ProviderTerminalFailure,
    /// Role or planner response violated the expected JSON/protocol contract.
    ProtocolFailure,
    /// File/tool loop failure.
    ToolFailure,
    /// Project validation command failed.
    ValidationFailure,
    /// Planner output violated structured planner validation.
    PlannerValidationFailure,
    /// Work producer accepted a result with no artifact file changes.
    WorkSemanticValidationFailure,
    /// Deliberation reached a semantic quality limit, such as exhausted revisions.
    DeliberationFailure,
    /// Artifact integration failed.
    IntegrationFailure,
    /// Artifact integration was refused because the branch tip advanced past
    /// the workspace's base commit (a sibling node integrated first).
    IntegrationConflict,
    /// The user task was semantically rejected by the producing role.
    UserTaskRejection,
    /// The node's dispatch thread panicked before returning a result.
    DispatchPanic,
}

/// The recovery action that triggered an `AttemptsExhausted` failure.
///
/// Stored on `FailureReason::AttemptsExhausted` so callers can distinguish
/// which kind of recovery exhausted the attempt budget without string parsing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExhaustedAction {
    /// The node's attempt budget was consumed by `Retry` recovery.
    Retry,
    /// The node's attempt budget was consumed by `Split` recovery.
    Split,
    /// The node's attempt budget was consumed by `ElevateModel` recovery.
    ElevateModel,
}

impl fmt::Display for ExhaustedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retry => write!(f, "Retry"),
            Self::Split => write!(f, "Split"),
            Self::ElevateModel => write!(f, "ElevateModel"),
        }
    }
}

/// The typed cause of a scheduler run failure.
///
/// Replaces the raw `reason: String` in `SchedulerState::Failed` and
/// `SchedulerTerminalOutput::Failed` so callers can distinguish failure causes without
/// string parsing. The `Display` impl produces a human-readable message for
/// telemetry and manifests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FailureReason {
    /// A graph invariant was violated (duplicate IDs, missing dependencies, orphaned origins).
    GraphInvariantViolation(String),
    /// A protocol error: wrong event for state, wrong outcome for node kind, ID mismatch, etc.
    ProtocolViolation(String),
    /// No node is ready to run but the graph is not yet complete; blocked dependency chain or cycle.
    Deadlock(String),
    /// A node exhausted all retry attempts under a recoverable recovery action.
    AttemptsExhausted {
        /// The ID of the node that exhausted its attempts.
        node_id: String,
        /// The attempt limit that was reached.
        max_attempts: u32,
        /// The recovery action that triggered the exhaustion check.
        recovery_action: ExhaustedAction,
    },
    /// ElevateModel was requested but no higher model tier exists and attempts are exhausted.
    NoHigherModelTierAvailable {
        /// The ID of the node that could not be elevated.
        node_id: String,
        /// The attempt limit at the time of failure.
        max_attempts: u32,
    },
    /// Adding recovery nodes would exceed the graph size limit.
    GraphCapacityExceeded {
        /// The maximum number of nodes permitted in a run graph.
        limit: usize,
    },
    /// A plan expansion would exceed the plan depth limit.
    PlanDepthExceeded {
        /// The maximum nesting depth permitted for plan nodes.
        limit: usize,
    },
    /// A terminal recovery action halted the run.
    TerminalRecovery {
        /// The message from the `Terminal` recovery action.
        terminal_message: String,
        /// The original failure message that triggered the terminal recovery.
        failure_message: String,
    },
    /// Required test targets were not completed before the run finished.
    RequiredTestTargetsMissing(String),
    /// A `ForTasks`-spawned node's target files could not be derived from its
    /// task's name, e.g. the task has no recorded name or no configured
    /// `name_target_rule` matched it.
    TargetDerivationFailed(String),
}

impl fmt::Display for FailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GraphInvariantViolation(detail) => write!(f, "{detail}"),
            Self::ProtocolViolation(detail) => write!(f, "protocol violation: {detail}"),
            Self::Deadlock(detail) => write!(f, "{detail}"),
            Self::AttemptsExhausted {
                node_id,
                max_attempts,
                recovery_action,
            } => {
                write!(
                    f,
                    "node {node_id} exhausted all {max_attempts} attempts ({recovery_action})"
                )
            }
            Self::NoHigherModelTierAvailable {
                node_id,
                max_attempts,
            } => {
                write!(
                    f,
                    "node {node_id} exhausted all {max_attempts} attempts; no higher model tier available"
                )
            }
            Self::GraphCapacityExceeded { limit } => {
                write!(f, "graph size limit exceeded; limit is {limit}")
            }
            Self::PlanDepthExceeded { limit } => {
                write!(f, "plan depth limit exceeded; limit is {limit}")
            }
            Self::TerminalRecovery {
                terminal_message,
                failure_message,
            } => {
                if terminal_message.is_empty() {
                    write!(f, "{failure_message}")
                } else if failure_message.is_empty()
                    || terminal_message == failure_message
                    || terminal_message.contains(failure_message.as_str())
                {
                    write!(f, "{terminal_message}")
                } else if failure_message.contains(terminal_message.as_str()) {
                    write!(f, "{failure_message}")
                } else {
                    write!(f, "{terminal_message}: {failure_message}")
                }
            }
            Self::RequiredTestTargetsMissing(detail) => write!(f, "{detail}"),
            Self::TargetDerivationFailed(detail) => write!(f, "{detail}"),
        }
    }
}
