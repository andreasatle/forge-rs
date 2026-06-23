use std::path::PathBuf;

use serde::Deserialize;

use crate::artifacts::file_ops::validate_relative_path;
use crate::artifacts::{ArtifactUpdate, ArtifactView, FileChange};

/// Policy controlling what a [`FileToolExecutor`] may do.
#[derive(Clone, Debug)]
pub struct FileToolPolicy {
    /// Whether write operations (`WriteFile`, `ReplaceText`, `DeleteFile`) are allowed.
    pub allow_writes: bool,
    /// Maximum bytes returned by a single `ReadFile` call before content is truncated.
    pub max_read_bytes: usize,
    /// Maximum bytes accepted by a single `WriteFile` or `ReplaceText` call.
    /// Requests over this limit are rejected without recording.
    pub max_write_bytes: usize,
    /// Maximum bytes in the serialised observation string returned to the model.
    pub max_observation_bytes: usize,
}

impl Default for FileToolPolicy {
    fn default() -> Self {
        Self {
            allow_writes: true,
            max_read_bytes: 64 * 1024,
            max_write_bytes: 256 * 1024,
            max_observation_bytes: 16 * 1024,
        }
    }
}

/// A tool operation the LLM can request against the artifact.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum FileToolRequest {
    /// List all files present in the artifact.
    ListFiles,
    /// Read the contents of a single file.
    ReadFile {
        /// Path relative to the artifact root.
        path: String,
    },
    /// Record a write (create or overwrite) to be applied during integration.
    WriteFile {
        /// Path relative to the artifact root.
        path: String,
        /// Complete file contents to write.
        content: String,
    },
    /// Record a text replacement to be applied during integration.
    ReplaceText {
        /// Path relative to the artifact root.
        path: String,
        /// Text to find (must occur exactly once).
        old: String,
        /// Replacement text.
        new: String,
    },
    /// Record a file deletion to be applied during integration.
    DeleteFile {
        /// Path relative to the artifact root.
        path: String,
    },
}

/// The result of executing a single file tool request.
#[derive(Debug, Eq, PartialEq)]
pub enum FileToolResponse {
    /// File paths returned by [`FileToolRequest::ListFiles`].
    FileList {
        /// Paths relative to the artifact root, sorted deterministically.
        paths: Vec<PathBuf>,
    },
    /// Contents returned by [`FileToolRequest::ReadFile`].
    FileContents {
        /// The requested path.
        path: String,
        /// UTF-8 file contents, possibly truncated at `max_read_bytes`.
        content: String,
    },
    /// Confirmation that a write, replace, or delete was recorded.
    UpdateRecorded {
        /// Human-readable description of the recorded change.
        description: String,
    },
    /// The request could not be fulfilled.
    Failed {
        /// Reason for the failure.
        reason: String,
    },
}

/// Executes file tool requests, delegating reads to an [`ArtifactView`] and
/// accumulating write operations as a pending [`ArtifactUpdate`].
pub struct FileToolExecutor {
    view: ArtifactView,
    update: ArtifactUpdate,
    policy: FileToolPolicy,
}

impl FileToolExecutor {
    /// Creates a new executor backed by `view` with the default policy
    /// (writes allowed, conservative size limits).
    pub fn new(view: ArtifactView) -> Self {
        Self::with_policy(view, FileToolPolicy::default())
    }

    /// Creates a new executor backed by `view` with an explicit `policy`.
    pub fn with_policy(view: ArtifactView, policy: FileToolPolicy) -> Self {
        Self {
            view,
            update: ArtifactUpdate::default(),
            policy,
        }
    }

    /// Returns the policy governing this executor.
    pub fn policy(&self) -> &FileToolPolicy {
        &self.policy
    }

    /// Executes a tool request and returns the result.
    ///
    /// Read operations (`ListFiles`, `ReadFile`) call through to the artifact
    /// view immediately. Write operations (`WriteFile`, `ReplaceText`,
    /// `DeleteFile`) validate the path and then append to the pending update
    /// without touching the artifact. The pending update is retrieved with
    /// [`FileToolExecutor::into_update`].
    pub fn execute(&mut self, request: FileToolRequest) -> FileToolResponse {
        match request {
            FileToolRequest::ListFiles => match self.view.list_files() {
                Ok(paths) => FileToolResponse::FileList { paths },
                Err(e) => FileToolResponse::Failed {
                    reason: e.to_string(),
                },
            },

            FileToolRequest::ReadFile { path } => match self.view.read_file(&path) {
                Ok(content) => {
                    if content.len() > self.policy.max_read_bytes {
                        let truncated =
                            truncate_to_char_boundary(&content, self.policy.max_read_bytes);
                        FileToolResponse::FileContents {
                            path,
                            content: format!(
                                "{}\n[truncated after {} bytes]",
                                truncated, self.policy.max_read_bytes,
                            ),
                        }
                    } else {
                        FileToolResponse::FileContents { path, content }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    let reason = if msg.contains("utf-8") || msg.contains("utf8") {
                        "binary or non-UTF-8 file cannot be read as text".to_string()
                    } else {
                        msg
                    };
                    FileToolResponse::Failed { reason }
                }
            },

            FileToolRequest::WriteFile { path, content } => {
                if !self.policy.allow_writes {
                    return FileToolResponse::Failed {
                        reason: "write operations are not permitted for this role".to_string(),
                    };
                }
                if content.len() > self.policy.max_write_bytes {
                    return FileToolResponse::Failed {
                        reason: format!(
                            "content too large: {} bytes exceeds the {} byte limit",
                            content.len(),
                            self.policy.max_write_bytes,
                        ),
                    };
                }
                if let Err(e) = validate_relative_path(&path) {
                    return FileToolResponse::Failed {
                        reason: e.to_string(),
                    };
                }
                let description = format!("write {path}");
                self.update
                    .changes
                    .push(FileChange::Write { path, content });
                FileToolResponse::UpdateRecorded { description }
            }

            FileToolRequest::ReplaceText { path, old, new } => {
                if !self.policy.allow_writes {
                    return FileToolResponse::Failed {
                        reason: "write operations are not permitted for this role".to_string(),
                    };
                }
                if new.len() > self.policy.max_write_bytes {
                    return FileToolResponse::Failed {
                        reason: format!(
                            "replacement text too large: {} bytes exceeds the {} byte limit",
                            new.len(),
                            self.policy.max_write_bytes,
                        ),
                    };
                }
                if let Err(e) = validate_relative_path(&path) {
                    return FileToolResponse::Failed {
                        reason: e.to_string(),
                    };
                }
                let description = format!("replace text in {path}");
                self.update
                    .changes
                    .push(FileChange::Replace { path, old, new });
                FileToolResponse::UpdateRecorded { description }
            }

            FileToolRequest::DeleteFile { path } => {
                if !self.policy.allow_writes {
                    return FileToolResponse::Failed {
                        reason: "write operations are not permitted for this role".to_string(),
                    };
                }
                if let Err(e) = validate_relative_path(&path) {
                    return FileToolResponse::Failed {
                        reason: e.to_string(),
                    };
                }
                let description = format!("delete {path}");
                self.update.changes.push(FileChange::Delete { path });
                FileToolResponse::UpdateRecorded { description }
            }
        }
    }

    /// Consumes the executor and returns all accumulated pending changes.
    pub fn into_update(self) -> ArtifactUpdate {
        self.update
    }
}

/// Truncates `s` to at most `max_bytes` bytes, staying at a UTF-8 character
/// boundary so the result is always a valid `&str`.
fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &s[..boundary]
}

/// Parses a JSON-encoded [`FileToolRequest`].
///
/// Returns `Err` with a human-readable message when the JSON is malformed or
/// names an unknown tool.
pub fn parse_tool_request(json: &str) -> Result<FileToolRequest, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::artifacts::{ArtifactUpdate, ArtifactView, FileChange};

    use super::*;

    // ── git fixture ──────────────────────────────────────────────────────────

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("forge-tools-{label}-{}-{id}", std::process::id()));
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

    // ── policy: producer can write ───────────────────────────────────────────

    #[test]
    fn producer_can_record_write_update() {
        let policy = FileToolPolicy {
            allow_writes: true,
            ..FileToolPolicy::default()
        };
        let mut executor = FileToolExecutor::with_policy(dummy_view(), policy);

        let response = executor.execute(FileToolRequest::WriteFile {
            path: "output.txt".to_owned(),
            content: "hello\n".to_owned(),
        });

        assert!(
            matches!(response, FileToolResponse::UpdateRecorded { .. }),
            "producer policy must allow write_file; got {response:?}"
        );
        let update = executor.into_update();
        assert_eq!(update.changes.len(), 1, "one change must be recorded");
        assert!(
            matches!(&update.changes[0], FileChange::Write { path, .. } if path == "output.txt"),
            "recorded change must be a Write to output.txt"
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
        assert_eq!(
            executor.into_update(),
            ArtifactUpdate::default(),
            "no update must be recorded on policy rejection"
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
        assert_eq!(
            executor.into_update(),
            ArtifactUpdate::default(),
            "no update must be recorded on policy rejection"
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
        assert_eq!(
            executor.into_update(),
            ArtifactUpdate::default(),
            "no update must be recorded on size rejection"
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
        assert_eq!(
            executor.into_update(),
            ArtifactUpdate::default(),
            "no update must be recorded on size rejection"
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

    // ── write-path tests (no git needed) ─────────────────────────────────────

    #[test]
    fn write_file_records_update_without_mutating_artifact() {
        let mut executor = FileToolExecutor::new(dummy_view());

        let response = executor.execute(FileToolRequest::WriteFile {
            path: "output.txt".to_owned(),
            content: "hello\n".to_owned(),
        });

        assert!(
            matches!(response, FileToolResponse::UpdateRecorded { .. }),
            "expected UpdateRecorded, got {response:?}"
        );
        let update = executor.into_update();
        assert_eq!(
            update.changes,
            vec![FileChange::Write {
                path: "output.txt".to_owned(),
                content: "hello\n".to_owned(),
            }]
        );
    }

    #[test]
    fn replace_text_records_update() {
        let mut executor = FileToolExecutor::new(dummy_view());

        let response = executor.execute(FileToolRequest::ReplaceText {
            path: "output.txt".to_owned(),
            old: "hello".to_owned(),
            new: "goodbye".to_owned(),
        });

        assert!(
            matches!(response, FileToolResponse::UpdateRecorded { .. }),
            "expected UpdateRecorded, got {response:?}"
        );
        let update = executor.into_update();
        assert_eq!(
            update.changes,
            vec![FileChange::Replace {
                path: "output.txt".to_owned(),
                old: "hello".to_owned(),
                new: "goodbye".to_owned(),
            }]
        );
    }

    #[test]
    fn delete_file_records_update() {
        let mut executor = FileToolExecutor::new(dummy_view());

        let response = executor.execute(FileToolRequest::DeleteFile {
            path: "old.txt".to_owned(),
        });

        assert!(
            matches!(response, FileToolResponse::UpdateRecorded { .. }),
            "expected UpdateRecorded, got {response:?}"
        );
        let update = executor.into_update();
        assert_eq!(
            update.changes,
            vec![FileChange::Delete {
                path: "old.txt".to_owned(),
            }]
        );
    }

    #[test]
    fn invalid_path_rejected_before_recording_update() {
        let mut executor = FileToolExecutor::new(dummy_view());

        let response = executor.execute(FileToolRequest::WriteFile {
            path: "../escape.txt".to_owned(),
            content: "bad\n".to_owned(),
        });

        assert!(
            matches!(response, FileToolResponse::Failed { .. }),
            "expected Failed for path traversal, got {response:?}"
        );
        assert_eq!(executor.into_update(), ArtifactUpdate::default());
    }

    #[test]
    fn into_update_returns_recorded_changes() {
        let mut executor = FileToolExecutor::new(dummy_view());
        executor.execute(FileToolRequest::WriteFile {
            path: "a.txt".to_owned(),
            content: "aaa\n".to_owned(),
        });
        executor.execute(FileToolRequest::DeleteFile {
            path: "b.txt".to_owned(),
        });

        let update = executor.into_update();

        assert_eq!(
            update.changes,
            vec![
                FileChange::Write {
                    path: "a.txt".to_owned(),
                    content: "aaa\n".to_owned(),
                },
                FileChange::Delete {
                    path: "b.txt".to_owned(),
                },
            ]
        );
    }

    // ── JSON parsing tests ───────────────────────────────────────────────────

    #[test]
    fn parse_list_files_tool_request() {
        let request = parse_tool_request(r#"{"tool":"list_files"}"#).unwrap();
        assert_eq!(request, FileToolRequest::ListFiles);
    }

    #[test]
    fn parse_read_file_tool_request() {
        let request = parse_tool_request(r#"{"tool":"read_file","path":"README.md"}"#).unwrap();
        assert_eq!(
            request,
            FileToolRequest::ReadFile {
                path: "README.md".to_owned(),
            }
        );
    }

    #[test]
    fn parse_write_file_tool_request() {
        let request =
            parse_tool_request(r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#)
                .unwrap();
        assert_eq!(
            request,
            FileToolRequest::WriteFile {
                path: "output.txt".to_owned(),
                content: "hello".to_owned(),
            }
        );
    }

    #[test]
    fn parse_replace_text_tool_request() {
        let request = parse_tool_request(
            r#"{"tool":"replace_text","path":"output.txt","old":"hello","new":"goodbye"}"#,
        )
        .unwrap();
        assert_eq!(
            request,
            FileToolRequest::ReplaceText {
                path: "output.txt".to_owned(),
                old: "hello".to_owned(),
                new: "goodbye".to_owned(),
            }
        );
    }

    #[test]
    fn parse_delete_file_tool_request() {
        let request = parse_tool_request(r#"{"tool":"delete_file","path":"old.txt"}"#).unwrap();
        assert_eq!(
            request,
            FileToolRequest::DeleteFile {
                path: "old.txt".to_owned(),
            }
        );
    }

    #[test]
    fn parse_unknown_tool_returns_error() {
        let result = parse_tool_request(r#"{"tool":"run_shell","cmd":"rm -rf /"}"#);
        assert!(result.is_err(), "unknown tool must fail to parse");
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        let result = parse_tool_request("not json");
        assert!(result.is_err(), "malformed JSON must fail to parse");
    }
}
