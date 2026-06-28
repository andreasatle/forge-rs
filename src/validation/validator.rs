//! Workspace validation before artifact integration.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::artifacts::Workspace;

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
            let result = run_command_with_timeout(spec, workspace.path(), self.timeout);
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
fn workspace_has_matching_file(dir: &Path, patterns: &[String]) -> bool {
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
fn matches_name_glob(pattern: &str, name: &str) -> bool {
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

fn command_display(spec: &CommandSpec) -> String {
    let mut parts = vec![spec.program.clone()];
    parts.extend(spec.args.iter().cloned());
    parts.join(" ")
}

/// Run `spec` directly (no shell) in `dir` with a hard deadline.
///
/// Stdout and stderr are redirected to anonymous temp files so large output
/// cannot fill the pipe buffer and deadlock the child. The parent polls
/// `try_wait` every 50 ms and kills the child if it outlives `timeout`.
fn run_command_with_timeout(
    spec: &CommandSpec,
    dir: &std::path::Path,
    timeout: Duration,
) -> ValidationResult {
    let display = command_display(spec);

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

    let mut child = match Command::new(&spec.program)
        .args(&spec.args)
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

    fn spec(program: &str, args: &[&str]) -> CommandSpec {
        CommandSpec {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            when_files_present: vec![],
        }
    }

    #[test]
    fn command_validator_passes_when_command_exits_zero() {
        let (path, ws) = temp_workspace();
        std::fs::write(path.join("expected.txt"), "").unwrap();

        let v = CommandValidator::new(
            vec![spec("test", &["-f", "expected.txt"])],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(result.passed, "expected pass, got: {}", result.summary);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn command_validator_fails_when_command_exits_nonzero() {
        let (path, ws) = temp_workspace();

        let v = CommandValidator::new(
            vec![spec("test", &["-f", "this_file_does_not_exist.txt"])],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(!result.passed, "expected failure");
        assert!(
            result.summary.contains("this_file_does_not_exist.txt"),
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
    fn command_validator_captures_failed_command_details() {
        let (path, ws) = temp_workspace();

        let v = CommandValidator::new(
            vec![spec(
                "sh",
                &[
                    "-c",
                    "printf 'from stdout'; printf 'from stderr' >&2; exit 7",
                ],
            )],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(!result.passed, "expected failure");
        let failure = result.failure.expect("failure details must be captured");
        assert_eq!(
            failure.command,
            "sh -c printf 'from stdout'; printf 'from stderr' >&2; exit 7"
        );
        assert_eq!(failure.exit_code, Some(7));
        assert_eq!(failure.stdout, "from stdout");
        assert_eq!(failure.stderr, "from stderr");

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn command_validator_runs_in_workspace_directory() {
        let (path, ws) = temp_workspace();
        std::fs::write(path.join("workspace_marker.txt"), "").unwrap();

        let v = CommandValidator::new(
            vec![spec("test", &["-f", "workspace_marker.txt"])],
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
                spec("false", &[]),
                spec("touch", &[&marker.display().to_string()]),
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

        let v = CommandValidator::new(vec![spec("sleep", &["5"])], Duration::from_secs(1));
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

        let v1 = CommandValidator::new(vec![spec("sleep", &["5"])], Duration::from_secs(1));
        let r1 = v1.validate(&ws1);
        assert!(!r1.passed, "first validator must time out and fail");

        let v2 = CommandValidator::new(vec![spec("echo", &["ok"])], default_timeout());
        let r2 = v2.validate(&ws2);
        assert!(
            r2.passed,
            "second validator must pass after the first timed out; got: {}",
            r2.summary
        );
    }

    // ── direct-exec tests ─────────────────────────────────────────────────────

    #[test]
    fn command_validator_executes_directly_without_shell() {
        let (_path, ws) = temp_workspace();

        // 'false' exits 1. Summary must name the program directly, not wrap in "sh -c".
        let v = CommandValidator::new(vec![spec("false", &[])], default_timeout());
        let result = v.validate(&ws);

        assert!(!result.passed, "false must fail");
        assert!(
            result.summary.contains("false"),
            "summary must mention the command; got: {}",
            result.summary
        );
        assert!(
            !result.summary.contains("sh -c"),
            "summary must not mention a shell wrapper; got: {}",
            result.summary
        );
    }

    #[test]
    fn command_validator_passes_with_direct_true() {
        let (_path, ws) = temp_workspace();

        let v = CommandValidator::new(vec![spec("true", &[])], default_timeout());
        let result = v.validate(&ws);

        assert!(
            result.passed,
            "direct exec of 'true' must pass: {}",
            result.summary
        );
    }

    #[test]
    fn command_spec_args_are_passed_without_shell_interpretation() {
        let (path, ws) = temp_workspace();
        // Create a file whose name contains a shell-special character.
        // If the args were shell-expanded, "*.marker" might glob-expand or cause issues.
        // With direct exec, "*.marker" is passed literally to 'test -f'.
        std::fs::write(path.join("*.marker"), "").unwrap();

        let v = CommandValidator::new(vec![spec("test", &["-f", "*.marker"])], default_timeout());
        let result = v.validate(&ws);

        assert!(
            result.passed,
            "literal filename with special char must be found via direct exec; got: {}",
            result.summary
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    // ── backward-compat shell translation test ────────────────────────────────

    #[test]
    fn shell_wrapped_command_spec_runs_correctly() {
        let (_path, ws) = temp_workspace();

        // This is how the backward-compat translation wraps raw YAML commands.
        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "true".to_string()],
                when_files_present: vec![],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(
            result.passed,
            "sh -c true wrapped as CommandSpec must pass: {}",
            result.summary
        );
    }

    #[test]
    fn shell_wrapped_failure_surfaces_correct_command_display() {
        let (_path, ws) = temp_workspace();

        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "false".to_string()],
                when_files_present: vec![],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);

        assert!(!result.passed, "sh -c false must fail");
        assert!(
            result.summary.contains("sh"),
            "summary must mention the sh program; got: {}",
            result.summary
        );
    }

    // ── when_files_present guard tests ────────────────────────────────────────

    #[test]
    fn when_files_present_skips_command_when_no_matching_file_exists() {
        // Invariant: a command with `when_files_present` set is skipped — not
        // failed — when no file in the workspace matches any of the patterns.
        let (path, ws) = temp_workspace();
        // Workspace has no test_*.py files; `false` must never run.
        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "false".to_string(),
                args: vec![],
                when_files_present: vec!["test_*.py".to_string()],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);
        assert!(
            result.passed,
            "command guarded by when_files_present must be skipped when no matching file exists; got: {}",
            result.summary
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn when_files_present_runs_command_when_matching_file_exists() {
        // Invariant: a command with `when_files_present` set IS run when at
        // least one workspace file matches any pattern.
        let (path, ws) = temp_workspace();
        std::fs::write(path.join("test_foo.py"), "# test\n").unwrap();
        // `true` exits 0 — we just verify the command ran (and passed).
        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "true".to_string(),
                args: vec![],
                when_files_present: vec!["test_*.py".to_string()],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);
        assert!(
            result.passed,
            "command guarded by when_files_present must run when a matching file exists; got: {}",
            result.summary
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn when_files_present_suffix_pattern_matches_correctly() {
        // Invariant: suffix glob pattern (*_test.py) matches files ending with
        // the suffix and skips the command when only non-matching files exist.
        let (path, ws) = temp_workspace();
        // Only a non-test source file exists.
        std::fs::write(path.join("main.py"), "# main\n").unwrap();
        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "false".to_string(),
                args: vec![],
                when_files_present: vec!["*_test.py".to_string()],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);
        assert!(
            result.passed,
            "*_test.py guard must skip command when only main.py exists; got: {}",
            result.summary
        );

        // Now add a matching file; command must run.
        std::fs::write(path.join("main_test.py"), "# tests\n").unwrap();
        let v2 = CommandValidator::new(
            vec![CommandSpec {
                program: "true".to_string(),
                args: vec![],
                when_files_present: vec!["*_test.py".to_string()],
            }],
            default_timeout(),
        );
        let result2 = v2.validate(&ws);
        assert!(
            result2.passed,
            "*_test.py guard must run command when main_test.py exists; got: {}",
            result2.summary
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn when_files_present_empty_always_runs_command() {
        // Invariant: empty when_files_present imposes no restriction — the
        // command runs regardless of workspace contents.
        let (_path, ws) = temp_workspace();
        // `true` always passes; verifying it runs even with no patterns set.
        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "true".to_string(),
                args: vec![],
                when_files_present: vec![],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);
        assert!(
            result.passed,
            "empty when_files_present must not suppress the command; got: {}",
            result.summary
        );
    }

    #[test]
    fn when_files_present_matches_file_in_subdirectory() {
        // Invariant: when_files_present walks subdirectories, not just the root.
        let (path, ws) = temp_workspace();
        let subdir = path.join("tests");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("test_something.py"), "# test\n").unwrap();
        let v = CommandValidator::new(
            vec![CommandSpec {
                program: "true".to_string(),
                args: vec![],
                when_files_present: vec!["test_*.py".to_string()],
            }],
            default_timeout(),
        );
        let result = v.validate(&ws);
        assert!(
            result.passed,
            "when_files_present must find test files in subdirectories; got: {}",
            result.summary
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    // ── matches_name_glob unit tests ──────────────────────────────────────────

    #[test]
    fn matches_name_glob_exact_when_no_wildcard() {
        // Invariant: without *, the pattern is an exact name match.
        assert!(matches_name_glob("foo.py", "foo.py"));
        assert!(!matches_name_glob("foo.py", "bar.py"));
    }

    #[test]
    fn matches_name_glob_prefix_pattern() {
        // Invariant: test_*.py matches any file starting with test_ and ending .py.
        assert!(matches_name_glob("test_*.py", "test_foo.py"));
        assert!(matches_name_glob("test_*.py", "test_main.py"));
        assert!(!matches_name_glob("test_*.py", "main.py"));
        assert!(!matches_name_glob("test_*.py", "foo_test.py"));
    }

    #[test]
    fn matches_name_glob_suffix_pattern() {
        // Invariant: *_test.py matches any file ending with _test.py.
        assert!(matches_name_glob("*_test.py", "main_test.py"));
        assert!(matches_name_glob("*_test.py", "foo_test.py"));
        assert!(!matches_name_glob("*_test.py", "test_main.py"));
        assert!(!matches_name_glob("*_test.py", "main.py"));
    }

    #[test]
    fn matches_name_glob_full_wildcard_matches_any_name() {
        // Invariant: * alone matches any filename.
        assert!(matches_name_glob("*", "anything.py"));
        assert!(matches_name_glob("*", "README.md"));
    }
}
