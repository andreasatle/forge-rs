//! Typed failure classification.

use crate::machines::scheduler::{FailureKind, RecoveryAction};

/// Classify a typed failure kind into the appropriate [`RecoveryAction`].
///
/// `message` is diagnostic text only. Do not parse it for recovery decisions.
pub fn classify_deliberation_failure(kind: FailureKind, message: &str) -> RecoveryAction {
    match kind {
        FailureKind::ProviderFailure | FailureKind::ProtocolFailure => RecoveryAction::Retry {
            message: format!("retryable failure: {message}"),
        },
        FailureKind::DeliberationFailure => RecoveryAction::ElevateModel {
            message: format!("semantic failure: {message}"),
        },
        FailureKind::ProviderTerminalFailure
        | FailureKind::ToolFailure
        | FailureKind::ValidationFailure
        | FailureKind::PlannerValidationFailure
        | FailureKind::WorkSemanticValidationFailure
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
    fn validation_failure_terminal_independent_of_message_text() {
        let a = classify_deliberation_failure(FailureKind::ValidationFailure, "validation failed");
        let b = classify_deliberation_failure(FailureKind::ValidationFailure, "tests did not pass");
        assert!(matches!(a, RecoveryAction::Terminal { .. }));
        assert!(matches!(b, RecoveryAction::Terminal { .. }));
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
