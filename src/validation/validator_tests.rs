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
        scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
            scope: ValidationScope::Workspace,
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
fn matches_name_glob_handles_supported_patterns() {
    let cases = [
        ("exact match", "foo.py", "foo.py", true),
        ("exact mismatch", "foo.py", "bar.py", false),
        (
            "prefix wildcard first match",
            "test_*.py",
            "test_foo.py",
            true,
        ),
        (
            "prefix wildcard second match",
            "test_*.py",
            "test_main.py",
            true,
        ),
        (
            "prefix wildcard missing prefix",
            "test_*.py",
            "main.py",
            false,
        ),
        (
            "prefix wildcard wrong suffix position",
            "test_*.py",
            "foo_test.py",
            false,
        ),
        (
            "suffix wildcard first match",
            "*_test.py",
            "main_test.py",
            true,
        ),
        (
            "suffix wildcard second match",
            "*_test.py",
            "foo_test.py",
            true,
        ),
        (
            "suffix wildcard wrong prefix position",
            "*_test.py",
            "test_main.py",
            false,
        ),
        (
            "suffix wildcard missing suffix",
            "*_test.py",
            "main.py",
            false,
        ),
        ("full wildcard python file", "*", "anything.py", true),
        ("full wildcard markdown file", "*", "README.md", true),
    ];

    for (name, pattern, file_name, expected) in cases {
        assert_eq!(
            matches_name_glob(pattern, file_name),
            expected,
            "{name}: pattern={pattern:?}, file_name={file_name:?}"
        );
    }
}
