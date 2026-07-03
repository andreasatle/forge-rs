use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::artifacts::{Artifact, ArtifactView, WorkspaceFactory};

use super::*;

// ── git fixture ──────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("forge-tools-{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &PathBuf, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("failed to run git");
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn git_output(dir: &PathBuf, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(out.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Builds a bare repository containing a single committed file and returns
/// an `ArtifactView` pointing at that commit.
fn make_view(label: &str) -> (TempDir, ArtifactView) {
    let temp = TempDir::new(label);

    let seed = temp.0.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Tool Test"]);
    git(
        &seed,
        &["config", "user.email", "tool-test@example.invalid"],
    );
    fs::write(seed.join("hello.txt"), "hello world\n").unwrap();
    git(&seed, &["add", "hello.txt"]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);

    let bare = temp.0.join("bare.git");
    let status = Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("failed to clone bare repo");
    assert!(status.success(), "git clone --bare failed");

    let sha = git_output(&bare, &["rev-parse", "HEAD"]);
    let view = ArtifactView {
        repo_path: bare,
        commit_sha: sha,
    };
    (temp, view)
}

/// Builds a bare repository with a binary (non-UTF-8) file committed.
fn make_view_with_binary(label: &str) -> (TempDir, ArtifactView) {
    let temp = TempDir::new(label);

    let seed = temp.0.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Tool Test"]);
    git(
        &seed,
        &["config", "user.email", "tool-test@example.invalid"],
    );
    // Write bytes that are not valid UTF-8.
    fs::write(seed.join("binary.bin"), b"\x00\x01\x02\xFF\xFE").unwrap();
    git(&seed, &["add", "binary.bin"]);
    git(&seed, &["commit", "--quiet", "-m", "add binary"]);

    let bare = temp.0.join("bare.git");
    Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("failed to clone bare repo");

    let sha = git_output(&bare, &["rev-parse", "HEAD"]);
    let view = ArtifactView {
        repo_path: bare,
        commit_sha: sha,
    };
    (temp, view)
}

/// Returns an `ArtifactView` with a nonexistent path — safe to use only
/// when tests never exercise the read path.
fn dummy_view() -> ArtifactView {
    ArtifactView {
        repo_path: PathBuf::from("/nonexistent"),
        commit_sha: "deadbeef".to_owned(),
    }
}

fn workspace_executor(view: &ArtifactView, policy: FileToolPolicy) -> FileToolExecutor {
    let artifact = Artifact {
        repo_path: view.repo_path.clone(),
        branch: "main".to_string(),
        commit_sha: view.commit_sha.clone(),
    };
    let workspace = WorkspaceFactory::new(&artifact)
        .create_temporary_workspace()
        .unwrap();
    let workspace = Rc::new(RefCell::new(workspace));
    FileToolExecutor::with_workspace(view.clone(), workspace, policy)
}

// ── policy: producer can write ───────────────────────────────────────────

#[test]
fn producer_can_write_workspace_file() {
    let (_temp, view) = make_view("producer-write-workspace");
    let policy = FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    };
    let mut executor = workspace_executor(&view, policy);

    let response = executor.execute(FileToolRequest::WriteFile {
        path: "output.txt".to_owned(),
        content: "hello\n".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::UpdateRecorded { .. }),
        "producer policy must allow write_file; got {response:?}"
    );
    assert_eq!(
        executor.execute(FileToolRequest::ReadFile {
            path: "output.txt".to_owned(),
        }),
        FileToolResponse::FileContents {
            path: "output.txt".to_owned(),
            content: "hello\n".to_owned(),
        }
    );
}

#[test]
fn absolute_read_path_with_allowed_targets_returns_target_guidance() {
    let (_temp, view) = make_view("absolute-read-target-guidance");
    let policy = FileToolPolicy {
        allowed_paths: Some(vec!["main.py".to_string()]),
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(view, policy);

    let response = executor.execute(FileToolRequest::ReadFile {
        path: "/tmp/main.py".to_string(),
    });

    let FileToolResponse::Failed { reason } = response else {
        panic!("expected Failed for absolute path outside allowed targets; got {response:?}");
    };
    assert!(
        reason.contains("main.py"),
        "failure reason must name the allowed target path; got {reason:?}"
    );
}

#[test]
fn producer_cannot_write_untargeted_file() {
    let policy = FileToolPolicy {
        allow_writes: true,
        allowed_paths: Some(vec!["main.py".to_string()]),
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(dummy_view(), policy);

    let response = executor.execute(FileToolRequest::WriteFile {
        path: "other.py".to_string(),
        content: "print('no')\n".to_string(),
    });

    let FileToolResponse::Failed { reason } = response else {
        panic!("expected Failed for write outside allowed targets; got {response:?}");
    };
    assert!(
        reason.contains("main.py"),
        "failure reason must name the allowed target path; got {reason:?}"
    );
}

// ── policy: critic write is rejected ────────────────────────────────────

#[test]
fn critic_write_request_is_rejected() {
    let policy = FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(dummy_view(), policy);

    let response = executor.execute(FileToolRequest::WriteFile {
        path: "output.txt".to_owned(),
        content: "critic should not write".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::Failed { .. }),
        "read-only policy must reject write_file; got {response:?}"
    );
}

// ── policy: referee delete is rejected ──────────────────────────────────

#[test]
fn referee_delete_request_is_rejected() {
    let policy = FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(dummy_view(), policy);

    let response = executor.execute(FileToolRequest::DeleteFile {
        path: "old.txt".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::Failed { .. }),
        "read-only policy must reject delete_file; got {response:?}"
    );
}

// ── policy: read-only allows reads ──────────────────────────────────────

#[test]
fn read_only_policy_allows_read_file() {
    let (_temp, view) = make_view("read-only-allows-read");
    let policy = FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(view, policy);

    let response = executor.execute(FileToolRequest::ReadFile {
        path: "hello.txt".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::FileContents { .. }),
        "read-only policy must still allow read_file; got {response:?}"
    );
}

// ── read limit: large content is truncated ───────────────────────────────

#[test]
fn read_file_large_content_is_truncated() {
    let (_temp, view) = make_view("large-read");
    // hello.txt contains "hello world\n" (12 bytes); set a 5-byte read limit.
    let policy = FileToolPolicy {
        max_read_bytes: 5,
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(view, policy);

    let response = executor.execute(FileToolRequest::ReadFile {
        path: "hello.txt".to_owned(),
    });

    match response {
        FileToolResponse::FileContents { content, .. } => {
            assert!(
                content.starts_with("hello"),
                "truncated content must start with the first 5 bytes; got: {content:?}"
            );
            assert!(
                content.contains("[truncated after 5 bytes]"),
                "truncation marker must be present; got: {content:?}"
            );
        }
        other => panic!("expected FileContents, got {other:?}"),
    }
}

// ── write limit: oversized write is rejected ─────────────────────────────

#[test]
fn write_file_too_large_is_rejected() {
    let policy = FileToolPolicy {
        allow_writes: true,
        max_write_bytes: 10,
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(dummy_view(), policy);

    let response = executor.execute(FileToolRequest::WriteFile {
        path: "out.txt".to_owned(),
        content: "this content is longer than ten bytes".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::Failed { .. }),
        "oversized write must be rejected; got {response:?}"
    );
}

// ── write limit: oversized replace is rejected ───────────────────────────

#[test]
fn replace_text_too_large_is_rejected() {
    let policy = FileToolPolicy {
        allow_writes: true,
        max_write_bytes: 10,
        ..FileToolPolicy::default()
    };
    let mut executor = FileToolExecutor::with_policy(dummy_view(), policy);

    let response = executor.execute(FileToolRequest::ReplaceText {
        path: "out.txt".to_owned(),
        old: "old".to_owned(),
        new: "this replacement text is longer than ten bytes".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::Failed { .. }),
        "oversized replace must be rejected; got {response:?}"
    );
}

// ── binary read returns failure ──────────────────────────────────────────

#[test]
fn binary_read_returns_failure() {
    let (_temp, view) = make_view_with_binary("binary-read");
    let mut executor = FileToolExecutor::new(view);

    let response = executor.execute(FileToolRequest::ReadFile {
        path: "binary.bin".to_owned(),
    });

    match response {
        FileToolResponse::Failed { reason } => {
            assert!(
                reason.contains("binary") || reason.contains("non-UTF-8"),
                "failure reason must describe the binary/encoding issue; got: {reason:?}"
            );
        }
        other => panic!("expected Failed for binary file, got {other:?}"),
    }
}

// ── read-path tests (require git) ────────────────────────────────────────

#[test]
fn list_files_uses_artifact_view() {
    let (_temp, view) = make_view("list-files");
    let mut executor = FileToolExecutor::new(view);

    let response = executor.execute(FileToolRequest::ListFiles);

    assert_eq!(
        response,
        FileToolResponse::FileList {
            paths: vec![PathBuf::from("hello.txt")],
        }
    );
}

#[test]
fn read_file_uses_artifact_view() {
    let (_temp, view) = make_view("read-file");
    let mut executor = FileToolExecutor::new(view);

    let response = executor.execute(FileToolRequest::ReadFile {
        path: "hello.txt".to_owned(),
    });

    assert_eq!(
        response,
        FileToolResponse::FileContents {
            path: "hello.txt".to_owned(),
            content: "hello world\n".to_owned(),
        }
    );
}

// ── workspace write-path tests ───────────────────────────────────────────

#[test]
fn write_file_mutates_workspace() {
    let (_temp, view) = make_view("write-workspace");
    let mut executor = workspace_executor(&view, FileToolPolicy::default());

    let response = executor.execute(FileToolRequest::WriteFile {
        path: "output.txt".to_owned(),
        content: "hello\n".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::UpdateRecorded { .. }),
        "expected UpdateRecorded, got {response:?}"
    );
    assert_eq!(
        executor.execute(FileToolRequest::ReadFile {
            path: "output.txt".to_owned(),
        }),
        FileToolResponse::FileContents {
            path: "output.txt".to_owned(),
            content: "hello\n".to_owned(),
        }
    );
}

#[test]
fn parsed_write_file_normalizes_literal_newline_escapes() {
    // (case name, JSON payload, expected file content after write)
    let cases = [
        (
            "real newline in JSON string is written as-is",
            r#"{"tool":"write_file","path":"output.txt","content":"first line\nsecond line\n"}"#,
            "first line\nsecond line\n",
        ),
        (
            "double-escaped \\n is normalized to a real newline",
            r#"{"tool":"write_file","path":"output.txt","content":"first line\\nsecond line"}"#,
            "first line\nsecond line",
        ),
    ];

    for (case, json, expected_content) in cases {
        let (_temp, view) = make_view("write-json-newline-normalization");
        let mut executor = workspace_executor(&view, FileToolPolicy::default());
        let request = parse_tool_request(json).unwrap();

        let response = executor.execute(request);

        assert!(
            matches!(response, FileToolResponse::UpdateRecorded { .. }),
            "{case}: expected UpdateRecorded, got {response:?}"
        );
        assert_eq!(
            executor.execute(FileToolRequest::ReadFile {
                path: "output.txt".to_owned(),
            }),
            FileToolResponse::FileContents {
                path: "output.txt".to_owned(),
                content: expected_content.to_owned(),
            },
            "{case}"
        );
    }
}

#[test]
fn replace_text_mutates_workspace() {
    let (_temp, view) = make_view("replace-workspace");
    let mut executor = workspace_executor(&view, FileToolPolicy::default());
    executor.execute(FileToolRequest::WriteFile {
        path: "output.txt".to_owned(),
        content: "hello world".to_owned(),
    });

    let response = executor.execute(FileToolRequest::ReplaceText {
        path: "output.txt".to_owned(),
        old: "hello".to_owned(),
        new: "goodbye".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::UpdateRecorded { .. }),
        "expected UpdateRecorded, got {response:?}"
    );
    assert_eq!(
        executor.execute(FileToolRequest::ReadFile {
            path: "output.txt".to_owned(),
        }),
        FileToolResponse::FileContents {
            path: "output.txt".to_owned(),
            content: "goodbye world".to_owned(),
        }
    );
}

#[test]
fn parsed_replace_text_does_not_normalize_literal_newline_escapes() {
    // (case name, baseline file content, JSON payload, expected content after replace)
    let cases = [
        (
            "real newline in JSON string matches and replaces as-is",
            "alpha\nbeta\ngamma\n",
            r#"{"tool":"replace_text","path":"output.txt","old":"alpha\nbeta","new":"one\ntwo"}"#,
            "one\ntwo\ngamma\n",
        ),
        (
            "double-escaped \\n is NOT normalized (unlike write_file)",
            r"alpha\nbeta tail",
            r#"{"tool":"replace_text","path":"output.txt","old":"alpha\\nbeta","new":"one\\ntwo"}"#,
            r"one\ntwo tail",
        ),
    ];

    for (case, baseline, json, expected_content) in cases {
        let (_temp, view) = make_view("replace-json-newline-normalization");
        let mut executor = workspace_executor(&view, FileToolPolicy::default());
        executor.execute(FileToolRequest::WriteFile {
            path: "output.txt".to_owned(),
            content: baseline.to_owned(),
        });
        let request = parse_tool_request(json).unwrap();

        let response = executor.execute(request);

        assert!(
            matches!(response, FileToolResponse::UpdateRecorded { .. }),
            "{case}: expected UpdateRecorded, got {response:?}"
        );
        assert_eq!(
            executor.execute(FileToolRequest::ReadFile {
                path: "output.txt".to_owned(),
            }),
            FileToolResponse::FileContents {
                path: "output.txt".to_owned(),
                content: expected_content.to_owned(),
            },
            "{case}"
        );
    }
}

#[test]
fn delete_file_mutates_workspace() {
    let (_temp, view) = make_view("delete-workspace");
    let mut executor = workspace_executor(&view, FileToolPolicy::default());

    let response = executor.execute(FileToolRequest::DeleteFile {
        path: "hello.txt".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::UpdateRecorded { .. }),
        "expected UpdateRecorded, got {response:?}"
    );
    assert!(matches!(
        executor.execute(FileToolRequest::ReadFile {
            path: "hello.txt".to_owned(),
        }),
        FileToolResponse::Failed { .. }
    ));
}

#[test]
fn invalid_path_rejected_before_mutating_workspace() {
    let mut executor = FileToolExecutor::new(dummy_view());

    let response = executor.execute(FileToolRequest::WriteFile {
        path: "../escape.txt".to_owned(),
        content: "bad\n".to_owned(),
    });

    assert!(
        matches!(response, FileToolResponse::Failed { .. }),
        "expected Failed for path traversal, got {response:?}"
    );
}

// ── JSON parsing tests ───────────────────────────────────────────────────

#[test]
fn parse_valid_tool_requests() {
    let cases = [
        (r#"{"tool":"list_files"}"#, FileToolRequest::ListFiles),
        (
            r#"{"tool":"read_file","path":"README.md"}"#,
            FileToolRequest::ReadFile {
                path: "README.md".to_owned(),
            },
        ),
        (
            r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#,
            FileToolRequest::WriteFile {
                path: "output.txt".to_owned(),
                content: "hello".to_owned(),
            },
        ),
        (
            r#"{"tool":"replace_text","path":"output.txt","old":"hello","new":"goodbye"}"#,
            FileToolRequest::ReplaceText {
                path: "output.txt".to_owned(),
                old: "hello".to_owned(),
                new: "goodbye".to_owned(),
            },
        ),
        (
            r#"{"tool":"delete_file","path":"old.txt"}"#,
            FileToolRequest::DeleteFile {
                path: "old.txt".to_owned(),
            },
        ),
        (
            r#"{"tool":"write_file","path":"output.txt","content":"hello world"}"#,
            FileToolRequest::WriteFile {
                path: "output.txt".to_owned(),
                content: "hello world".to_owned(),
            },
        ),
        (
            r#"{"tool":"replace_text","path":"f.txt","old":"hello","new":"goodbye"}"#,
            FileToolRequest::ReplaceText {
                path: "f.txt".to_owned(),
                old: "hello".to_owned(),
                new: "goodbye".to_owned(),
            },
        ),
    ];

    for (json, expected) in cases {
        assert_eq!(
            parse_tool_request(json),
            Ok(expected),
            "failed to parse valid tool request: {json}"
        );
    }
}

#[test]
fn parse_invalid_tool_requests() {
    let cases = [
        (
            "unknown tool",
            r#"{"tool":"run_shell","cmd":"rm -rf /"}"#,
            None,
        ),
        ("malformed JSON", "not json", None),
        (
            "replace_text placeholder old",
            r#"{"tool":"replace_text","path":"f.txt","old":"...","new":"x"}"#,
            Some("placeholder"),
        ),
        (
            "replace_text placeholder new",
            r#"{"tool":"replace_text","path":"f.txt","old":"x","new":"..."}"#,
            Some("placeholder"),
        ),
        (
            "write_file placeholder content",
            r#"{"tool":"write_file","path":"output.txt","content":"..."}"#,
            Some("placeholder"),
        ),
        (
            "write_file dollar placeholder path",
            r#"{"tool":"write_file","path":"$TARGET_FILE","content":"real content"}"#,
            Some("placeholder"),
        ),
        (
            "write_file dollar placeholder content",
            r#"{"tool":"write_file","path":"real.txt","content":"$FILE_CONTENT"}"#,
            Some("placeholder"),
        ),
    ];

    for (name, json, expected_error_fragment) in cases {
        let error = parse_tool_request(json).expect_err(&format!("{name} must fail to parse"));
        if let Some(fragment) = expected_error_fragment {
            assert!(
                error.contains(fragment),
                "{name} error must contain {fragment:?}; got {error:?}"
            );
        }
    }
}

// ── trailing-whitespace robustness ───────────────────────────────────────

#[test]
fn parse_tool_request_tolerates_surrounding_whitespace() {
    let cases = [
        (
            "trailing newline",
            "{\"tool\":\"list_files\"}\n",
            FileToolRequest::ListFiles,
        ),
        (
            "trailing spaces and tabs",
            "{\"tool\":\"list_files\"}  \t  ",
            FileToolRequest::ListFiles,
        ),
        (
            "write_file trailing newlines",
            "{\"tool\":\"write_file\",\"path\":\".gitignore\",\"content\":\"*.log\\n\"}\n\n",
            FileToolRequest::WriteFile {
                path: ".gitignore".to_owned(),
                content: "*.log\n".to_owned(),
            },
        ),
        (
            "leading whitespace",
            "  \n{\"tool\":\"list_files\"}",
            FileToolRequest::ListFiles,
        ),
    ];

    for (name, json, expected) in cases {
        assert_eq!(
            parse_tool_request(json),
            Ok(expected),
            "{name} must not cause parse failure"
        );
    }
}
