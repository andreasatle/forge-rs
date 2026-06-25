//! Deliberation failure classification.

use crate::machines::scheduler::RecoveryAction;

/// Classify a deliberation failure reason into the appropriate [`RecoveryAction`].
///
/// Classification is based on substring matching against the lower-cased reason.
/// The function is deliberately conservative: anything not recognised is treated
/// as `Terminal` so that unknown failures halt the run rather than looping.
///
/// # Classification order
///
/// Order matters — earlier rules take priority over later ones.
///
/// 1. Fatal configuration / auth errors → `Terminal`
/// 2. Transient provider errors → `Retry`
/// 3. Task-shape / decomposition errors → `Split`
/// 4. Semantic quality errors → `ElevateModel`
/// 5. Anything else → `Terminal`
///
/// Auth/config failures are checked first so that a message like
/// "invalid api key: scope too large" does not accidentally trigger `Split`.
/// Provider transient failures are checked before task-shape signals for the
/// same reason.
///
/// # Classification table
///
/// | Pattern                          | Recovery     |
/// |----------------------------------|--------------|
/// | authentication failed            | Terminal     |
/// | invalid api key                  | Terminal     |
/// | configuration error              | Terminal     |
/// | unknown provider                 | Terminal     |
/// | timeout / timed out              | Retry        |
/// | connection refused / reset       | Retry        |
/// | temporarily unavailable          | Retry        |
/// | rate limit / 429                 | Retry        |
/// | provider error (retryable)       | Retry        |
/// | task too large                   | Split        |
/// | objective too broad              | Split        |
/// | needs decomposition              | Split        |
/// | split this task                  | Split        |
/// | too many files                   | Split        |
/// | scope too large                  | Split        |
/// | cannot solve as one task         | Split        |
/// | validation rejected              | ElevateModel |
/// | revision limit reached/exhausted | ElevateModel |
/// | critic rejected                  | ElevateModel |
/// | referee rejected                 | ElevateModel |
/// | anything else                    | Terminal     |
pub fn classify_deliberation_failure(reason: &str) -> RecoveryAction {
    let lower = reason.to_lowercase();

    // 1. Fatal configuration / auth errors — checked first so that incidental
    //    words like "scope" in an auth message do not trigger Split.
    if lower.contains("authentication failed")
        || lower.contains("invalid api key")
        || lower.contains("configuration error")
        || lower.contains("unknown provider")
    {
        return RecoveryAction::Terminal {
            message: format!("unrecoverable failure: {reason}"),
        };
    }

    // 2. Transient provider errors.
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

    // 3. Task-shape / decomposition signals.
    if lower.contains("task too large")
        || lower.contains("objective too broad")
        || lower.contains("needs decomposition")
        || lower.contains("split this task")
        || lower.contains("too many files")
        || lower.contains("scope too large")
        || lower.contains("cannot solve as one task")
    {
        return RecoveryAction::Split {
            message: format!("split requested after failure: {reason}"),
        };
    }

    // 4. Semantic quality errors — cheap model was not capable enough.
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

    // 5. Fail safe.
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
    fn critic_rejected_prefixed_reason_classified_as_elevate_model() {
        // This is the format emitted by DeliberationMachine when the Critic rejects.
        assert!(matches!(
            classify_deliberation_failure(
                "critic rejected: the haiku is not following the 5-7-5 syllable structure"
            ),
            RecoveryAction::ElevateModel { .. }
        ));
    }

    #[test]
    fn critic_rejected_semantic_reason_without_prefix_is_terminal() {
        // Verifies that a raw semantic critique without the "critic rejected:" prefix
        // still falls through to Terminal — the prefix is load-bearing.
        assert!(matches!(
            classify_deliberation_failure(
                "the haiku is not following the 5-7-5 syllable structure"
            ),
            RecoveryAction::Terminal { .. }
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

    // --- Split cases ---

    #[test]
    fn task_too_large_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("task too large to complete in a single pass"),
            RecoveryAction::Split { .. }
        ));
    }

    #[test]
    fn objective_too_broad_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("objective too broad: cannot determine starting point"),
            RecoveryAction::Split { .. }
        ));
    }

    #[test]
    fn needs_decomposition_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("this objective needs decomposition into sub-tasks"),
            RecoveryAction::Split { .. }
        ));
    }

    #[test]
    fn split_this_task_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("split this task into smaller pieces"),
            RecoveryAction::Split { .. }
        ));
    }

    #[test]
    fn too_many_files_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("too many files to process in one node"),
            RecoveryAction::Split { .. }
        ));
    }

    #[test]
    fn scope_too_large_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("scope too large for a single work node"),
            RecoveryAction::Split { .. }
        ));
    }

    #[test]
    fn cannot_solve_as_one_task_classified_as_split() {
        assert!(matches!(
            classify_deliberation_failure("cannot solve as one task"),
            RecoveryAction::Split { .. }
        ));
    }

    // --- precedence tests ---

    #[test]
    fn provider_timeout_with_split_words_still_retry() {
        // "task too large" appears in the message, but the leading "timeout"
        // pattern must win because transient errors are checked before split.
        assert!(matches!(
            classify_deliberation_failure("timeout: task too large to finish in time"),
            RecoveryAction::Retry { .. }
        ));
    }

    #[test]
    fn invalid_api_key_with_split_words_still_terminal() {
        // "scope too large" appears, but auth errors are checked first.
        assert!(matches!(
            classify_deliberation_failure("invalid api key: scope too large"),
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
