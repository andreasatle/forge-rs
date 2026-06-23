//! Workspace validation before artifact integration.

use std::process::Command;
use std::time::Duration;

use crate::artifacts::Workspace;

/// Outcome of a workspace validation pass.
pub struct ValidationResult {
    /// Whether validation passed.
    pub passed: bool,
    /// Human-readable summary of the result.
    pub summary: String,
}

/// Validates a workspace before artifact integration.
pub trait Validator {
    /// Inspect `workspace` and return whether it is safe to integrate.
    fn validate(&self, workspace: &Workspace) -> ValidationResult;
}

/// A no-op validator that always passes.
///
/// Used as the default when no validator is configured.
pub struct AlwaysPassValidator;

impl Validator for AlwaysPassValidator {
    fn validate(&self, _workspace: &Workspace) -> ValidationResult {
        ValidationResult {
            passed: true,
            summary: "validation passed".to_string(),
        }
    }
}

/// Validates a workspace by running shell commands inside it.
///
/// Commands run in order via `sh -c`; validation stops on the first failure.
/// Both stdout and stderr are captured and included in the failure summary.
pub struct CommandValidator {
    commands: Vec<String>,
    // TODO: enforce per-command timeout — stored but not yet applied
    #[allow(dead_code)]
    timeout: Duration,
}

impl CommandValidator {
    /// Create a new `CommandValidator` with the given commands and timeout.
    pub fn new(commands: Vec<String>, timeout: Duration) -> Self {
        Self { commands, timeout }
    }
}

impl Validator for CommandValidator {
    fn validate(&self, workspace: &Workspace) -> ValidationResult {
        for command in &self.commands {
            let output = Command::new("sh")
                .args(["-c", command])
                .current_dir(workspace.path())
                .output();

            match output {
                Err(e) => {
                    return ValidationResult {
                        passed: false,
                        summary: format!("command `{command}` failed to start: {e}"),
                    };
                }
                Ok(out) if !out.status.success() => {
                    let code = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string());
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return ValidationResult {
                        passed: false,
                        summary: format!(
                            "command `{command}` failed (exit {code})\nstdout: {stdout}\nstderr: {stderr}"
                        ),
                    };
                }
                Ok(_) => {}
            }
        }

        ValidationResult {
            passed: true,
            summary: format!("all {} command(s) passed", self.commands.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::Workspace;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> (PathBuf, Workspace) {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("forge-validator-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let ws = Workspace::at_path(path.clone(), "abc".to_string());
        (path, ws)
    }

    fn default_timeout() -> Duration {
        Duration::from_secs(30)
    }

    #[test]
    fn command_validator_passes_when_command_exits_zero() {
        let (path, ws) = temp_workspace();
        std::fs::write(path.join("expected.txt"), "").unwrap();

        let v = CommandValidator::new(vec!["test -f expected.txt".to_string()], default_timeout());
        let result = v.validate(&ws);

        assert!(result.passed, "expected pass, got: {}", result.summary);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn command_validator_fails_when_command_exits_nonzero() {
        let (path, ws) = temp_workspace();

        let v = CommandValidator::new(
            vec!["test -f this_file_does_not_exist.txt".to_string()],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(!result.passed, "expected failure");
        assert!(
            result
                .summary
                .contains("test -f this_file_does_not_exist.txt"),
            "summary must include the failing command, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("exit 1") || result.summary.contains("failed"),
            "summary must include exit status, got: {}",
            result.summary
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn command_validator_runs_in_workspace_directory() {
        let (path, ws) = temp_workspace();
        std::fs::write(path.join("workspace_marker.txt"), "").unwrap();

        // If cwd is the workspace path, this relative test will succeed.
        let v = CommandValidator::new(
            vec!["test -f workspace_marker.txt".to_string()],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(
            result.passed,
            "command must run inside workspace directory; got: {}",
            result.summary
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn command_validator_stops_on_first_failure() {
        let (path, ws) = temp_workspace();
        let marker = path.join("second_ran.txt");

        let v = CommandValidator::new(
            vec![
                "false".to_string(),
                // This would create the marker file, but must never run.
                format!("touch {}", marker.display()),
            ],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(!result.passed, "first command must fail validation");
        assert!(
            !marker.exists(),
            "second command must not run after first failure"
        );

        let _ = std::fs::remove_dir_all(&path);
    }
}
