//! Deliberation failure classification.

use crate::machines::scheduler::RecoveryAction;

/// Classify a deliberation failure reason into the appropriate [`RecoveryAction`].
///
/// Classification is based on substring matching against the lower-cased reason.
/// The function is deliberately conservative: anything not recognised as a
/// transient or semantic failure is treated as `Terminal` so that unknown
/// failures halt the run rather than looping indefinitely.
///
/// # Classification table
///
/// | Pattern                        | Recovery       |
/// |--------------------------------|----------------|
/// | timeout / timed out            | Retry          |
/// | connection refused / reset     | Retry          |
/// | temporarily unavailable        | Retry          |
/// | rate limit / 429               | Retry          |
/// | provider error (retryable)     | Retry          |
/// | validation rejected            | ElevateModel   |
/// | revision limit reached         | ElevateModel   |
/// | critic rejected                | ElevateModel   |
/// | referee rejected               | ElevateModel   |
/// | anything else                  | Terminal       |
pub fn classify_deliberation_failure(reason: &str) -> RecoveryAction {
    let lower = reason.to_lowercase();

    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("temporarily unavailable")
        || lower.contains("rate limit")
        || lower.contains("429")
        || lower.contains("provider error (retryable)")
    {
        return RecoveryAction::Retry {
            message: format!("transient failure: {reason}"),
        };
    }

    if lower.contains("validation rejected")
        || lower.contains("revision limit reached")
        || lower.contains("revision limit exhausted")
        || lower.contains("critic rejected")
        || lower.contains("referee rejected")
    {
        return RecoveryAction::ElevateModel {
            message: format!("semantic failure: {reason}"),
        };
    }

    RecoveryAction::Terminal {
        message: format!("unrecoverable failure: {reason}"),
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

    // --- Retry cases ---

    #[test]
    fn timeout_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("request timeout after 30s"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn timed_out_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("operation timed out"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn connection_refused_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure(
                "connection refused on http://localhost:11434/completion"
            ),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn connection_reset_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("connection reset by peer"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn temporarily_unavailable_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("service temporarily unavailable"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn rate_limit_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("rate limit exceeded, try again later"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn http_429_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("HTTP 429 Too Many Requests"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn provider_error_retryable_classified_as_retry() {
        assert!(matches!(
            classify_deliberation_failure("provider error (Retryable): upstream timeout"),
            RecoveryAction::Retry { .. }
        ));
    }

    // --- ElevateModel cases ---

    #[test]
    fn validation_rejected_classified_as_elevate_model() {
        assert!(matches!(
            classify_deliberation_failure("validation rejected: output did not satisfy schema"),
            RecoveryAction::ElevateModel { .. }
        ));
    }

    #[test]
    fn revision_limit_reached_classified_as_elevate_model() {
        assert!(matches!(
            classify_deliberation_failure("revision limit reached after 3 attempts"),
            RecoveryAction::ElevateModel { .. }
        ));
    }

    #[test]
    fn revision_limit_exhausted_classified_as_elevate_model() {
        assert!(matches!(
            classify_deliberation_failure("revision limit exhausted: content did not satisfy bar"),
            RecoveryAction::ElevateModel { .. }
        ));
    }

    #[test]
    fn critic_rejected_classified_as_elevate_model() {
        assert!(matches!(
            classify_deliberation_failure("critic rejected the producer output"),
            RecoveryAction::ElevateModel { .. }
        ));
    }

    #[test]
    fn referee_rejected_classified_as_elevate_model() {
        assert!(matches!(
            classify_deliberation_failure("referee rejected: content did not meet quality bar"),
            RecoveryAction::ElevateModel { .. }
        ));
    }

    // --- Terminal cases ---

    #[test]
    fn authentication_failed_classified_as_terminal() {
        assert!(matches!(
            classify_deliberation_failure("authentication failed"),
            RecoveryAction::Terminal { .. }
        ));
    }

    #[test]
    fn invalid_api_key_classified_as_terminal() {
        assert!(matches!(
            classify_deliberation_failure("invalid api key"),
            RecoveryAction::Terminal { .. }
        ));
    }

    #[test]
    fn configuration_error_classified_as_terminal() {
        assert!(matches!(
            classify_deliberation_failure("configuration error: missing model name"),
            RecoveryAction::Terminal { .. }
        ));
    }

    #[test]
    fn unknown_provider_classified_as_terminal() {
        assert!(matches!(
            classify_deliberation_failure("unknown provider: gpt-99"),
            RecoveryAction::Terminal { .. }
        ));
    }

    #[test]
    fn unknown_failure_classified_as_terminal() {
        assert!(matches!(
            classify_deliberation_failure("something completely unexpected happened"),
            RecoveryAction::Terminal { .. }
        ));
    }

    // --- label helper ---

    #[test]
    fn recovery_label_returns_correct_strings() {
        assert_eq!(
            recovery_label(&RecoveryAction::Retry {
                message: String::new()
            }),
            "Retry"
        );
        assert_eq!(
            recovery_label(&RecoveryAction::ElevateModel {
                message: String::new()
            }),
            "ElevateModel"
        );
        assert_eq!(
            recovery_label(&RecoveryAction::Split {
                message: String::new()
            }),
            "Split"
        );
        assert_eq!(
            recovery_label(&RecoveryAction::Terminal {
                message: String::new()
            }),
            "Terminal"
        );
    }
}
