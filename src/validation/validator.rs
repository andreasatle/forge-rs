//! Workspace validation before artifact integration.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::artifacts::Workspace;

/// Determines whether a validation command runs against node-local file paths
/// or the full workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationScope {
    /// Append the node's declared target files to the command.
    TargetFiles,
    /// Append the files changed by the node's artifact update to the command.
    ChangedFiles,
    /// Run the command exactly as declared against the full workspace.
    Workspace,
}

fn default_validation_scope() -> ValidationScope {
    ValidationScope::Workspace
}

/// A structured command specification executed directly without a shell.
///
/// The program is invoked via [`Command::new`]; args are passed as-is and are
/// never interpreted by a shell. Use `CommandSpec { program: "sh".into(), args:
/// vec!["-c".into(), cmd] }` to run shell syntax when needed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CommandSpec {
    /// The program to execute.
    pub program: String,
    /// Arguments passed to the program without shell interpretation.
    #[serde(default)]
    pub args: Vec<String>,
    /// When non-empty, the command is skipped unless at least one file in the
    /// workspace matches one of these simple glob patterns (supports a single
    /// `*` wildcard matching any sequence of characters in the file name).
    ///
    /// An empty list imposes no restriction — the command always runs.
    ///
    /// Example: `["test_*.py", "*_test.py"]` skips the command when no Python
    /// test files are present in the workspace.
    #[serde(default)]
    pub when_files_present: Vec<String>,
    /// Validation scope used when this command is stamped into a
    /// node-owned [`crate::validation::ValidationPlan`].
    ///
    /// The legacy [`CommandValidator`] treats every command as workspace
    /// scoped and does not append paths.
    #[serde(default = "default_validation_scope")]
    pub scope: ValidationScope,
}

impl CommandSpec {
    /// Human-readable command line for display in summaries and errors.
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }

    /// Run this command directly (no shell) in `dir` with a hard deadline.
    ///
    /// Stdout and stderr are redirected to anonymous temp files so large output
    /// cannot fill the pipe buffer and deadlock the child. The parent polls
    /// `try_wait` every 50 ms and kills the child if it outlives `timeout`.
    pub(crate) fn run_with_timeout(
        &self,
        dir: &std::path::Path,
        timeout: Duration,
    ) -> ValidationResult {
        let display = self.display();

        let mut stdout_file = match tempfile::tempfile() {
            Ok(f) => f,
            Err(e) => {
                return ValidationResult {
                    passed: false,
                    summary: format!("command `{display}` failed to start: {e}"),
                    failure: Some(ValidationCommandFailure {
                        command: display,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    }),
                };
            }
        };
        let mut stderr_file = match tempfile::tempfile() {
            Ok(f) => f,
            Err(e) => {
                return ValidationResult {
                    passed: false,
                    summary: format!("command `{display}` failed to start: {e}"),
                    failure: Some(ValidationCommandFailure {
                        command: display,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    }),
                };
            }
        };

        let stdout_fd = match stdout_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                return ValidationResult {
                    passed: false,
                    summary: format!("command `{display}` failed to start: {e}"),
                    failure: Some(ValidationCommandFailure {
                        command: display,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    }),
                };
            }
        };
        let stderr_fd = match stderr_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                return ValidationResult {
                    passed: false,
                    summary: format!("command `{display}` failed to start: {e}"),
                    failure: Some(ValidationCommandFailure {
                        command: display,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    }),
                };
            }
        };

        let mut child = match Command::new(&self.program)
            .args(&self.args)
            .current_dir(dir)
            .stdout(Stdio::from(stdout_fd))
            .stderr(Stdio::from(stderr_fd))
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ValidationResult {
                    passed: false,
                    summary: format!("command `{display}` failed to start: {e}"),
                    failure: Some(ValidationCommandFailure {
                        command: display,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    }),
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
                        summary: format!("command `{display}` failed to start: {e}"),
                        failure: Some(ValidationCommandFailure {
                            command: display,
                            exit_code: None,
                            stdout: String::new(),
                            stderr: e.to_string(),
                        }),
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
                            failure: None,
                        };
                    }
                    let exit_code = status.code();
                    let code = exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string());
                    return ValidationResult {
                        passed: false,
                        summary: format!(
                            "command `{display}` failed (exit {code})\nstdout: {stdout}\nstderr: {stderr}"
                        ),
                        failure: Some(ValidationCommandFailure {
                            command: display,
                            exit_code,
                            stdout,
                            stderr,
                        }),
                    };
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        stdout_file.seek(SeekFrom::Start(0)).ok();
                        stderr_file.seek(SeekFrom::Start(0)).ok();
                        let mut stdout = String::new();
                        let mut stderr = String::new();
                        stdout_file.read_to_string(&mut stdout).ok();
                        stderr_file.read_to_string(&mut stderr).ok();
                        let secs = timeout.as_secs();
                        if stderr.is_empty() {
                            stderr = format!("timed out after {secs} seconds");
                        }
                        return ValidationResult {
                            passed: false,
                            summary: format!(
                                "validation command timed out after {secs} seconds\ncommand:\n{display}"
                            ),
                            failure: Some(ValidationCommandFailure {
                                command: display,
                                exit_code: None,
                                stdout,
                                stderr,
                            }),
                        };
                    }
                    std::thread::sleep(poll);
                }
            }
        }
    }
}

/// Outcome of a workspace validation pass.
pub struct ValidationResult {
    /// Whether validation passed.
    pub passed: bool,
    /// Human-readable summary of the result.
    pub summary: String,
    /// Structured details for the first failed validation command, when any.
    pub failure: Option<ValidationCommandFailure>,
}

/// Structured details for a failed validation command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationCommandFailure {
    /// Human-readable command line.
    pub command: String,
    /// Process exit code. `None` means the command did not produce a normal exit
    /// status, such as timeout, signal termination, or spawn failure.
    pub exit_code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
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
            failure: None,
        }
    }
}

/// Validates a workspace by running [`CommandSpec`] commands directly inside it.
///
/// Commands run in order; validation stops on the first failure.
/// Each command gets its own independent timeout budget.
/// Commands are executed via [`Command::new`] — no shell is involved.
pub struct CommandValidator {
    commands: Vec<CommandSpec>,
    timeout: Duration,
}

impl CommandValidator {
    /// Create a new `CommandValidator` with the given command specs and timeout.
    pub fn new(commands: Vec<CommandSpec>, timeout: Duration) -> Self {
        Self { commands, timeout }
    }
}

impl Validator for CommandValidator {
    fn validate(&self, workspace: &Workspace) -> ValidationResult {
        let mut ran = 0usize;
        for spec in &self.commands {
            if !spec.when_files_present.is_empty()
                && !workspace_has_matching_file(workspace.path(), &spec.when_files_present)
            {
                continue;
            }
            ran += 1;
            let result = spec.run_with_timeout(workspace.path(), self.timeout);
            if !result.passed {
                return result;
            }
        }

        ValidationResult {
            passed: true,
            summary: format!("all {ran} command(s) passed"),
            failure: None,
        }
    }
}

/// Returns `true` if at least one file in the workspace directory tree has a
/// name matching any of `patterns` using simple glob syntax (single `*`).
pub(crate) fn workspace_has_matching_file(dir: &Path, patterns: &[String]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && workspace_has_matching_file(&path, patterns) {
                return true;
            }
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && patterns.iter().any(|p| matches_name_glob(p, name))
        {
            return true;
        }
    }
    false
}

/// Match `name` against a simple glob `pattern` that supports a single `*`
/// wildcard expanding to any (possibly empty) sequence of characters.
///
/// If the pattern contains no `*`, it is treated as an exact match.
/// Only the file name component is matched — no path separators.
pub(crate) fn matches_name_glob(pattern: &str, name: &str) -> bool {
    match pattern.find('*') {
        None => pattern == name,
        Some(star) => {
            let prefix = &pattern[..star];
            let suffix = &pattern[star + 1..];
            // Suffix must not itself contain another `*` to keep the logic simple.
            // For the patterns we need (test_*.py, *_test.py), this is sufficient.
            if suffix.contains('*') {
                // Fall back to "just check prefix" for multi-wildcard patterns.
                name.starts_with(prefix)
            } else {
                name.starts_with(prefix)
                    && name.ends_with(suffix)
                    && name.len() >= prefix.len() + suffix.len()
            }
        }
    }
}

#[cfg(test)]
#[path = "validator_tests.rs"]
mod tests;
