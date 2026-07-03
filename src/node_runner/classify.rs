//! Typed failure classification.

use crate::machines::scheduler::{FailureKind, RecoveryAction};

/// Classify a typed failure kind into the appropriate [`RecoveryAction`].
///
/// `message` is diagnostic text only. Do not parse it for recovery decisions.
pub fn classify_deliberation_failure(kind: FailureKind, message: &str) -> RecoveryAction {
    match kind {
        FailureKind::ProviderFailure
        | FailureKind::ProtocolFailure
        | FailureKind::ValidationFailure => RecoveryAction::Retry {
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
        FailureKind::DeliberationFailure => RecoveryAction::ElevateModel {
            message: format!("semantic failure: {message}"),
        },
        FailureKind::ProviderTerminalFailure
        | FailureKind::ToolFailure
        | FailureKind::PlannerValidationFailure
        | FailureKind::IntegrationFailure
        | FailureKind::UserTaskRejection => RecoveryAction::Terminal {
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
            FailureKind::UserTaskRejection,
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
