//! Integration validation diagnostics.

use crate::validation::ValidationCommandFailure;

pub(crate) fn validation_retry_message(
    summary: &str,
    failure: Option<&ValidationCommandFailure>,
) -> String {
    let Some(failure) = failure else {
        return format!(
            "validation failed\nsummary: {}\ninstruction: fix the existing file using file tools before accepting",
            one_line(summary)
        );
    };

    let exit = failure
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "none".to_string());
    let diagnostics = diagnostic_lines(failure);
    let location = first_diagnostic_location(&diagnostics).unwrap_or("(not detected)");
    format!(
        "validation failed\ncommand: {command}\nexit code: {exit}\nfirst location: {location}\ndiagnostics:\n{diagnostics}\ninstruction: fix the existing file using file tools before accepting",
        command = failure.command
    )
}

fn diagnostic_lines(failure: &ValidationCommandFailure) -> String {
    let stderr_lines = nonempty_lines(&failure.stderr);
    let source = if stderr_lines.is_empty() {
        nonempty_lines(&failure.stdout)
    } else {
        stderr_lines
    };
    let lines = source.into_iter().take(5).collect::<Vec<_>>();
    if lines.is_empty() {
        "(no diagnostic output)".to_string()
    } else {
        lines.join("\n")
    }
}

fn nonempty_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn first_diagnostic_location(diagnostics: &str) -> Option<&str> {
    diagnostics.lines().find_map(|line| {
        let line = line.trim();
        let location = line.strip_prefix("-->").map(str::trim).unwrap_or(line);
        if looks_like_location(location) {
            Some(
                location
                    .split_whitespace()
                    .next()
                    .unwrap_or(location)
                    .trim_end_matches(':'),
            )
        } else if line.starts_with("File \"") {
            Some(line)
        } else {
            None
        }
    })
}

fn looks_like_location(line: &str) -> bool {
    let candidate = line
        .split_whitespace()
        .next()
        .unwrap_or(line)
        .trim_end_matches(':');
    let mut colon_parts = candidate.rsplitn(3, ':');
    let Some(last) = colon_parts.next() else {
        return false;
    };
    let Some(prev) = colon_parts.next() else {
        return false;
    };
    if !last.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if prev.chars().all(|c| c.is_ascii_digit()) {
        return colon_parts.next().is_some();
    }
    !prev.is_empty()
}

fn one_line(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    trimmed.lines().next().unwrap_or(trimmed).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_message_summarizes_long_validation_output() {
        let stderr = (1..=20)
            .map(|line| format!("main.py:{line}:1: syntax diagnostic {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let failure = ValidationCommandFailure {
            command: "uv run ruff check main.py".to_string(),
            exit_code: Some(1),
            stdout: "stdout line that should not appear after stderr fills the budget".to_string(),
            stderr,
        };

        let message = validation_retry_message("ruff failed", Some(&failure));

        assert!(message.contains("command: uv run ruff check main.py"));
        assert!(message.contains("exit code: 1"));
        assert!(message.contains("first location: main.py:1:1"));
        assert!(
            message
                .contains("instruction: fix the existing file using file tools before accepting")
        );
        assert!(message.contains("main.py:5:1: syntax diagnostic 5"));
        assert!(!message.contains("main.py:6:1: syntax diagnostic 6"));
        assert!(!message.contains("stdout line that should not appear"));
    }

    #[test]
    fn retry_message_includes_first_file_line_location() {
        let failure = ValidationCommandFailure {
            command: "python -m py_compile main.py".to_string(),
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "  File \"main.py\", line 1\n    def broken(:\nSyntaxError: invalid syntax"
                .to_string(),
        };

        let message = validation_retry_message("compile failed", Some(&failure));

        assert!(message.contains("first location: File \"main.py\", line 1"));
        assert!(message.contains("SyntaxError: invalid syntax"));
    }
}
