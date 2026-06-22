use std::path::PathBuf;

use serde::Deserialize;

use crate::artifacts::file_ops::validate_relative_path;
use crate::artifacts::{ArtifactUpdate, ArtifactView, FileChange};

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
        /// UTF-8 file contents.
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
}

impl FileToolExecutor {
    /// Creates a new executor backed by `view`.
    pub fn new(view: ArtifactView) -> Self {
        Self {
            view,
            update: ArtifactUpdate::default(),
        }
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
                Ok(content) => FileToolResponse::FileContents { path, content },
                Err(e) => FileToolResponse::Failed {
                    reason: e.to_string(),
                },
            },

            FileToolRequest::WriteFile { path, content } => {
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

    /// Returns an `ArtifactView` with a nonexistent path — safe to use only
    /// when tests never exercise the read path.
    fn dummy_view() -> ArtifactView {
        ArtifactView {
            repo_path: PathBuf::from("/nonexistent"),
            commit_sha: "deadbeef".to_owned(),
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
