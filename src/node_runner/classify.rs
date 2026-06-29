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
    fn provider_failure_retries_independent_of_message_text() {
        let a = classify_deliberation_failure(FailureKind::ProviderFailure, "timeout");
        let b = classify_deliberation_failure(FailureKind::ProviderFailure, "renamed diagnostic");
        assert!(matches!(a, RecoveryAction::Retry { .. }));
        assert!(matches!(b, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn protocol_failure_retries_independent_of_message_text() {
        let a = classify_deliberation_failure(
            FailureKind::ProtocolFailure,
            "protocol failure after write",
        );
        let b = classify_deliberation_failure(
            FailureKind::ProtocolFailure,
            "model returned invalid output",
        );
        assert!(matches!(a, RecoveryAction::Retry { .. }));
        assert!(matches!(b, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn deliberation_failure_elevates_independent_of_message_text() {
        let a = classify_deliberation_failure(
            FailureKind::DeliberationFailure,
            "revision limit exhausted",
        );
        let b = classify_deliberation_failure(
            FailureKind::DeliberationFailure,
            "quality gate did not converge",
        );
        assert!(matches!(a, RecoveryAction::ElevateModel { .. }));
        assert!(matches!(b, RecoveryAction::ElevateModel { .. }));
    }

    #[test]
    fn validation_failure_retries_independent_of_message_text() {
        let a = classify_deliberation_failure(FailureKind::ValidationFailure, "validation failed");
        let b = classify_deliberation_failure(FailureKind::ValidationFailure, "tests did not pass");
        assert!(matches!(a, RecoveryAction::Retry { .. }));
        assert!(matches!(b, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn work_semantic_validation_failure_retries_independent_of_message_text() {
        let a = classify_deliberation_failure(
            FailureKind::WorkSemanticValidationFailure,
            "accepted work did not produce an artifact update",
        );
        let b = classify_deliberation_failure(
            FailureKind::WorkSemanticValidationFailure,
            "semantic validation wording changed",
        );
        assert!(matches!(a, RecoveryAction::Retry { .. }));
        assert!(matches!(b, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn invalid_work_attempt_update_failure_retries_by_kind_not_message() {
        let action = classify_deliberation_failure(
            FailureKind::WorkSemanticValidationFailure,
            "replacement target not found",
        );

        assert!(
            matches!(action, RecoveryAction::Retry { .. }),
            "invalid WorkAttempt workspace updates must retry because of the typed kind"
        );
    }

    #[test]
    fn work_semantic_validation_retry_message_mentions_file_tool() {
        let action = classify_deliberation_failure(
            FailureKind::WorkSemanticValidationFailure,
            "diagnostic text can change",
        );
        let RecoveryAction::Retry { message } = action else {
            panic!("expected Retry");
        };
        assert!(
            message.contains("must modify the artifact"),
            "retry message must explain artifact mutation requirement; got: {message}"
        );
        assert!(
            message.contains("Use write_file by default")
                && message.contains("replacing most or all of an existing file"),
            "retry message must recommend write_file for whole-file rewrites; got: {message}"
        );
        assert!(
            message.contains("Use replace_text only for small, localized edits")
                && message.contains("exact old string that occurs once"),
            "retry message must restrict replace_text to exact localized edits; got: {message}"
        );
        assert!(
            message.contains("whitespace, indentation, or formatting differences"),
            "retry message must explain replace_text exact matching; got: {message}"
        );
        assert!(
            message.contains("cannot be validated after a failed replace_text")
                && message.contains("switch to write_file")
                && message.contains("instead of retrying another replace_text"),
            "retry message must tell Producer how to recover from invalid WorkAttempt updates; got: {message}"
        );
    }

    #[test]
    fn user_task_rejection_terminal_independent_of_message_text() {
        let a =
            classify_deliberation_failure(FailureKind::UserTaskRejection, "cannot do this task");
        let b = classify_deliberation_failure(
            FailureKind::UserTaskRejection,
            "semantic rejection wording changed",
        );
        assert!(matches!(a, RecoveryAction::Terminal { .. }));
        assert!(matches!(b, RecoveryAction::Terminal { .. }));
    }

    #[test]
    fn planner_validation_failure_terminal_independent_of_message_text() {
        let a = classify_deliberation_failure(
            FailureKind::PlannerValidationFailure,
            "planner output validation failed: self-dependency in task: t1",
        );
        let b = classify_deliberation_failure(
            FailureKind::PlannerValidationFailure,
            "structural validation error (rewording should not change recovery)",
        );
        assert!(matches!(a, RecoveryAction::Terminal { .. }));
        assert!(matches!(b, RecoveryAction::Terminal { .. }));
    }

    #[test]
    fn tool_failure_terminal_independent_of_message_text() {
        let a = classify_deliberation_failure(FailureKind::ToolFailure, "tool loop limit reached");
        let b = classify_deliberation_failure(
            FailureKind::ToolFailure,
            "tool loop limit exceeded after 5 steps",
        );
        assert!(matches!(a, RecoveryAction::Terminal { .. }));
        assert!(matches!(b, RecoveryAction::Terminal { .. }));
    }

    #[test]
    fn integration_failure_terminal_independent_of_message_text() {
        let a = classify_deliberation_failure(
            FailureKind::IntegrationFailure,
            "workspace creation failed",
        );
        let b = classify_deliberation_failure(
            FailureKind::IntegrationFailure,
            "artifact apply error: git conflict",
        );
        assert!(matches!(a, RecoveryAction::Terminal { .. }));
        assert!(matches!(b, RecoveryAction::Terminal { .. }));
    }

    #[test]
    fn provider_terminal_failure_terminal_independent_of_message_text() {
        let a = classify_deliberation_failure(
            FailureKind::ProviderTerminalFailure,
            "authentication failed",
        );
        let b = classify_deliberation_failure(
            FailureKind::ProviderTerminalFailure,
            "invalid API key: unauthorized",
        );
        assert!(matches!(a, RecoveryAction::Terminal { .. }));
        assert!(matches!(b, RecoveryAction::Terminal { .. }));
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
