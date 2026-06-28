//! Integration validation diagnostics.

use crate::validation::ValidationCommandFailure;

pub(crate) fn validation_retry_message(
    summary: &str,
    failure: Option<&ValidationCommandFailure>,
) -> String {
    let Some(failure) = failure else {
        return format!("validation failed: {}", concise_text(summary));
    };

    let exit = failure
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".to_string());
    let stdout = concise_text(&failure.stdout);
    let stderr = concise_text(&failure.stderr);
    format!(
        "validation failed\nprevious validation command: {command}\nexit code: {exit}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        command = failure.command
    )
}

fn concise_text(text: &str) -> String {
    const LIMIT: usize = 1200;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(LIMIT).collect();
    out.push_str("\n[truncated]");
    out
}
