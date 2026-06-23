//! Workspace validation before artifact integration.

use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
/// Each command gets its own independent timeout budget.
pub struct CommandValidator {
    commands: Vec<String>,
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
            let result = run_with_timeout(command, workspace.path(), self.timeout);
            if !result.passed {
                return result;
            }
        }

        ValidationResult {
            passed: true,
            summary: format!("all {} command(s) passed", self.commands.len()),
        }
    }
}

/// Run `command` via `sh -c` in `dir` with a hard deadline.
///
/// Stdout and stderr are redirected to anonymous temp files so large output
/// cannot fill the pipe buffer and deadlock the child. The parent polls
/// `try_wait` every 50 ms and kills the child if it outlives `timeout`.
fn run_with_timeout(command: &str, dir: &std::path::Path, timeout: Duration) -> ValidationResult {
    let mut stdout_file = match tempfile::tempfile() {
        Ok(f) => f,
        Err(e) => {
            return ValidationResult {
                passed: false,
                summary: format!("command `{command}` failed to start: {e}"),
            };
        }
    };
    let mut stderr_file = match tempfile::tempfile() {
        Ok(f) => f,
        Err(e) => {
            return ValidationResult {
                passed: false,
                summary: format!("command `{command}` failed to start: {e}"),
            };
        }
    };

    let stdout_fd = match stdout_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return ValidationResult {
                passed: false,
                summary: format!("command `{command}` failed to start: {e}"),
            };
        }
    };
    let stderr_fd = match stderr_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return ValidationResult {
                passed: false,
                summary: format!("command `{command}` failed to start: {e}"),
            };
        }
    };

    let mut child = match Command::new("sh")
        .args(["-c", command])
        .current_dir(dir)
        .stdout(Stdio::from(stdout_fd))
        .stderr(Stdio::from(stderr_fd))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ValidationResult {
                passed: false,
                summary: format!("command `{command}` failed to start: {e}"),
            };
        }
    };

    let poll = Duration::from_millis(50);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Err(e) => {
                return ValidationResult {
                    passed: false,
                    summary: format!("command `{command}` failed to start: {e}"),
                };
            }
            Ok(Some(status)) => {
                stdout_file.seek(SeekFrom::Start(0)).ok();
                stderr_file.seek(SeekFrom::Start(0)).ok();
                let mut stdout = String::new();
                let mut stderr = String::new();
                stdout_file.read_to_string(&mut stdout).ok();
                stderr_file.read_to_string(&mut stderr).ok();

                if status.success() {
                    return ValidationResult {
                        passed: true,
                        summary: String::new(),
                    };
                }
                let code = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                return ValidationResult {
                    passed: false,
                    summary: format!(
                        "command `{command}` failed (exit {code})\nstdout: {stdout}\nstderr: {stderr}"
                    ),
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let secs = timeout.as_secs();
                    return ValidationResult {
                        passed: false,
                        summary: format!(
                            "validation command timed out after {secs} seconds\ncommand:\n{command}"
                        ),
                    };
                }
                std::thread::sleep(poll);
            }
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

    #[test]
    fn command_validator_fails_when_command_times_out() {
        let (_path, ws) = temp_workspace();

        let v = CommandValidator::new(vec!["sleep 5".to_string()], Duration::from_secs(1));
        let result = v.validate(&ws);

        assert!(!result.passed, "timed-out command must fail validation");
        assert!(
            result.summary.contains("timed out"),
            "summary must mention timeout; got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("1 second"),
            "summary must include the timeout duration; got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("sleep 5"),
            "summary must include the command string; got: {}",
            result.summary
        );
    }

    #[test]
    fn timeout_does_not_prevent_later_validations() {
        let (_path1, ws1) = temp_workspace();
        let (_path2, ws2) = temp_workspace();

        // First validator times out.
        let v1 = CommandValidator::new(vec!["sleep 5".to_string()], Duration::from_secs(1));
        let r1 = v1.validate(&ws1);
        assert!(!r1.passed, "first validator must time out and fail");

        // Second validator must still work normally.
        let v2 = CommandValidator::new(vec!["echo ok".to_string()], default_timeout());
        let r2 = v2.validate(&ws2);
        assert!(
            r2.passed,
            "second validator must pass after the first timed out; got: {}",
            r2.summary
        );
    }
}
