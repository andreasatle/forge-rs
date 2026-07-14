//! Typed failure classification.

use crate::machines::scheduler::{FailureKind, RecoveryAction};

/// Classify a typed failure kind into the appropriate [`RecoveryAction`].
///
/// `message` is diagnostic text only. Do not parse it for recovery decisions.
pub fn classify_deliberation_failure(kind: FailureKind, message: &str) -> RecoveryAction {
    match kind {
        FailureKind::ProviderFailure
        | FailureKind::ProtocolFailure
        | FailureKind::ValidationFailure
        | FailureKind::IntegrationConflict => RecoveryAction::Retry {
            message: format!("retryable failure: {message}"),
        },
        FailureKind::WorkSemanticValidationFailure => RecoveryAction::Retry {
            message: format!(
                "retryable work semantic validation failure: {message}. Accepted Work results \
                 must modify the artifact in the current WorkAttempt workspace. Use write_file \
                 by default when creating a file or replacing most or all of an existing file. \
                 Use replace_text only for small, localized edits after reading the file and \
                 providing an exact old string that occurs once; whitespace, indentation, or \
                 formatting differences will cause replace_text to fail. If a workspace \
                 mutation cannot be validated after a failed replace_text, switch to write_file \
                 for whole-file rewrites instead of retrying another replace_text."
            ),
        },
        FailureKind::DeliberationFailure | FailureKind::PlannerValidationFailure => {
            RecoveryAction::Split {
                message: format!("semantic failure: {message}"),
            }
        }
        FailureKind::ProviderTerminalFailure
        | FailureKind::ToolFailure
        | FailureKind::IntegrationFailure
        | FailureKind::UserTaskRejection
        | FailureKind::DispatchPanic => RecoveryAction::Terminal {
            message: format!("unrecoverable failure: {message}"),
        },
    }
}

/// Return a short label for a `RecoveryAction` suitable for telemetry.
pub fn recovery_label(action: &RecoveryAction) -> &'static str {
    match action {
        RecoveryAction::Retry { .. } => "Retry",
        RecoveryAction::ElevateModel { .. } => "ElevateModel",
        RecoveryAction::Split { .. } => "Split",
        RecoveryAction::Terminal { .. } => "Terminal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_validation_failure_maps_to_split_not_terminal() {
        // Invariant: a Plan node's structured planner validation failure gets
        // a scheduler-level re-plan attempt (Split), the same escape hatch
        // DeliberationFailure already gets, instead of ending the whole run.
        // The planner already had real in-deliberation retry attempts with
        // feedback before this terminal deliberation outcome was reached, so
        // treating it as unconditionally unrecoverable (as it was before)
        // discards a self-correctable failure class.
        let recovery =
            classify_deliberation_failure(FailureKind::PlannerValidationFailure, "missing test");
        assert!(
            matches!(recovery, RecoveryAction::Split { .. }),
            "expected Split, got {recovery:?}"
        );
    }

    #[test]
    fn same_kind_same_recovery_regardless_of_message() {
        let kinds = [
            FailureKind::ProviderFailure,
            FailureKind::ProviderTerminalFailure,
            FailureKind::ProtocolFailure,
            FailureKind::ToolFailure,
            FailureKind::ValidationFailure,
            FailureKind::PlannerValidationFailure,
            FailureKind::WorkSemanticValidationFailure,
            FailureKind::DeliberationFailure,
            FailureKind::IntegrationFailure,
            FailureKind::IntegrationConflict,
            FailureKind::UserTaskRejection,
            FailureKind::DispatchPanic,
        ];
        for kind in kinds {
            let a = classify_deliberation_failure(kind, "original wording");
            let b = classify_deliberation_failure(kind, "completely different message text");
            assert_eq!(
                std::mem::discriminant(&a),
                std::mem::discriminant(&b),
                "same FailureKind {:?} must produce same RecoveryAction variant regardless of message",
                kind
            );
        }
    }
}
