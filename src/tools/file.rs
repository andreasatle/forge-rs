use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use serde::Deserialize;

use crate::artifacts::file_ops::validate_relative_path;
use crate::artifacts::{ArtifactError, ArtifactRead, Workspace, WorkspaceFileOps};
use crate::services::extract_json_object;

/// Policy controlling what a [`FileToolExecutor`] may do.
#[derive(Clone, Debug)]
pub struct FileToolPolicy {
    /// Whether write operations (`WriteFile`, `ReplaceText`, `DeleteFile`) are allowed.
    pub allow_writes: bool,
    /// Optional allow-list of paths this executor may read or mutate.
    ///
    /// When present, path-bearing file tools must use one of these exact
    /// relative paths. Path containment validation still runs before this
    /// allow-list check.
    pub allowed_paths: Option<Vec<String>>,
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
            allowed_paths: None,
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
    ///
    /// This is the default write tool when creating a file or replacing most or
    /// all of an existing file.
    WriteFile {
        /// Path relative to the artifact root.
        path: String,
        /// Complete file contents to write.
        content: String,
    },
    /// Record a text replacement to be applied during integration.
    ///
    /// Intended only for small, localized edits after the caller has read the
    /// file and can provide an exact, unique `old` string. Whitespace,
    /// indentation, or formatting differences cause the match to fail.
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

/// Executes file tool requests, delegating reads to an [`ArtifactRead`] source
/// and writes to a WorkAttempt workspace when mutation is allowed.
pub struct FileToolExecutor {
    view: Box<dyn ArtifactRead>,
    workspace: Option<Rc<RefCell<Workspace>>>,
    policy: FileToolPolicy,
    changed: bool,
}

impl FileToolExecutor {
    /// Creates a read-only executor backed by `view` with the default policy.
    pub fn new(view: impl ArtifactRead + 'static) -> Self {
        Self::with_policy(view, FileToolPolicy::default())
    }

    /// Creates a read-only executor backed by `view` with an explicit `policy`.
    ///
    /// Write requests fail unless the executor was built with
    /// [`FileToolExecutor::with_workspace`].
    pub fn with_policy(view: impl ArtifactRead + 'static, policy: FileToolPolicy) -> Self {
        Self {
            view: Box::new(view),
            workspace: None,
            policy,
            changed: false,
        }
    }

    /// Creates an executor that writes directly into an attempt workspace.
    pub fn with_workspace(
        view: impl ArtifactRead + 'static,
        workspace: Rc<RefCell<Workspace>>,
        policy: FileToolPolicy,
    ) -> Self {
        Self {
            view: Box::new(view),
            workspace: Some(workspace),
            policy,
            changed: false,
        }
    }

    fn read_content(&self, path: &str) -> Result<String, ArtifactError> {
        if let Some(workspace) = &self.workspace {
            return workspace.borrow().read_file(path);
        }
        self.view.read_file(path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        if let Some(workspace) = &self.workspace {
            return Ok(workspace.borrow().list_files());
        }
        self.view.list_files()
    }

    /// Returns the policy governing this executor.
    pub fn policy(&self) -> &FileToolPolicy {
        &self.policy
    }

    fn validate_path_allowed(&self, path: &str) -> Result<(), String> {
        validate_relative_path(path).map_err(|e| match (&self.policy.allowed_paths, e) {
            (Some(allowed), ArtifactError::PathOutsideWorkspace) => {
                allowed_target_guidance(allowed)
            }
            (_, e) => e.to_string(),
        })?;
        if let Some(allowed) = &self.policy.allowed_paths
            && !allowed.iter().any(|target| target == path)
        {
            return Err(allowed_target_guidance(allowed));
        }
        Ok(())
    }

    /// Executes a tool request and returns the result.
    ///
    pub fn execute(&mut self, request: FileToolRequest) -> FileToolResponse {
        match request {
            FileToolRequest::ListFiles => match self.list_files() {
                Ok(paths) => FileToolResponse::FileList { paths },
                Err(e) => FileToolResponse::Failed {
                    reason: e.to_string(),
                },
            },

            FileToolRequest::ReadFile { path } => {
                if let Err(reason) = self.validate_path_allowed(&path) {
                    return FileToolResponse::Failed { reason };
                }
                match self.read_content(&path) {
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
                    Err(ArtifactError::Encoding) => FileToolResponse::Failed {
                        reason: "binary or non-UTF-8 file cannot be read as text".to_string(),
                    },
                    Err(e) => FileToolResponse::Failed {
                        reason: e.to_string(),
                    },
                }
            }

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
                if let Err(reason) = self.validate_path_allowed(&path) {
                    return FileToolResponse::Failed { reason };
                }
                let description = format!("write {path}");
                if let Some(workspace) = &self.workspace {
                    match workspace.borrow_mut().write_file(&path, &content) {
                        Ok(()) => self.changed = true,
                        Err(e) => {
                            return FileToolResponse::Failed {
                                reason: e.to_string(),
                            };
                        }
                    }
                } else {
                    return FileToolResponse::Failed {
                        reason: "write operations require a WorkAttempt workspace".to_string(),
                    };
                }
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
                if let Err(reason) = self.validate_path_allowed(&path) {
                    return FileToolResponse::Failed { reason };
                }
                let content = match self.read_content(&path) {
                    Ok(c) => c,
                    Err(ArtifactError::Encoding) => {
                        return FileToolResponse::Failed {
                            reason: "binary or non-UTF-8 file cannot be read as text".to_string(),
                        };
                    }
                    Err(e) => {
                        return FileToolResponse::Failed {
                            reason: e.to_string(),
                        };
                    }
                };
                let mut occurrences = content.match_indices(old.as_str());
                let Some((start, _)) = occurrences.next() else {
                    return FileToolResponse::Failed {
                        reason: "replacement target not found".to_string(),
                    };
                };
                if occurrences.next().is_some() {
                    return FileToolResponse::Failed {
                        reason: "replacement target occurs more than once".to_string(),
                    };
                }
                let mut updated = String::with_capacity(content.len() - old.len() + new.len());
                updated.push_str(&content[..start]);
                updated.push_str(&new);
                updated.push_str(&content[start + old.len()..]);
                let description = format!("replace text in {path}");
                if let Some(workspace) = &self.workspace {
                    match workspace.borrow_mut().write_file(&path, &updated) {
                        Ok(()) => self.changed = true,
                        Err(e) => {
                            return FileToolResponse::Failed {
                                reason: e.to_string(),
                            };
                        }
                    }
                } else {
                    return FileToolResponse::Failed {
                        reason: "write operations require a WorkAttempt workspace".to_string(),
                    };
                }
                FileToolResponse::UpdateRecorded { description }
            }

            FileToolRequest::DeleteFile { path } => {
                if !self.policy.allow_writes {
                    return FileToolResponse::Failed {
                        reason: "write operations are not permitted for this role".to_string(),
                    };
                }
                if let Err(reason) = self.validate_path_allowed(&path) {
                    return FileToolResponse::Failed { reason };
                }
                let description = format!("delete {path}");
                if let Some(workspace) = &self.workspace {
                    match workspace.borrow_mut().delete_file(&path) {
                        Ok(()) => self.changed = true,
                        Err(e) => {
                            return FileToolResponse::Failed {
                                reason: e.to_string(),
                            };
                        }
                    }
                } else {
                    return FileToolResponse::Failed {
                        reason: "write operations require a WorkAttempt workspace".to_string(),
                    };
                }
                FileToolResponse::UpdateRecorded { description }
            }
        }
    }

    /// Returns whether this executor wrote to its backing state.
    pub fn changed(&self) -> bool {
        self.changed
    }
}

fn allowed_target_guidance(allowed: &[String]) -> String {
    format!(
        "Use a relative path from the allowed target list: {}",
        allowed.join(", ")
    )
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
/// Trims leading/trailing whitespace and extracts the first complete JSON object
/// before parsing, so provider artifacts or trailing newlines after the closing
/// brace are silently ignored.
///
/// Returns `Err` with a human-readable message when the JSON is malformed,
/// names an unknown tool, or contains placeholder `"..."` values that indicate
/// the model echoed a prompt example rather than issuing a real request.
pub fn parse_tool_request(json: &str) -> Result<FileToolRequest, String> {
    let json = json.trim();
    let json = extract_json_object(json)
        .ok_or_else(|| "no JSON object found in tool request".to_string())?;
    let req: FileToolRequest = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if has_placeholder_fields(&req) {
        return Err("tool request contains placeholder values".to_string());
    }
    Ok(req)
}

/// Returns `true` if `s` exactly matches a framework placeholder (`$[A-Z_]+`).
fn is_dollar_placeholder(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('$') && s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

fn has_placeholder_fields(req: &FileToolRequest) -> bool {
    match req {
        FileToolRequest::ListFiles => false,
        FileToolRequest::ReadFile { path } => path.trim() == "...",
        FileToolRequest::WriteFile { path, content } => {
            path.trim() == "..."
                || content.trim() == "..."
                || is_dollar_placeholder(path)
                || is_dollar_placeholder(content)
        }
        FileToolRequest::ReplaceText { path, old, new } => {
            path.trim() == "..." || old.trim() == "..." || new.trim() == "..."
        }
        FileToolRequest::DeleteFile { path } => path.trim() == "...",
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::artifacts::{Artifact, ArtifactView, create_temporary_workspace};

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

    fn workspace_executor(view: &ArtifactView, policy: FileToolPolicy) -> FileToolExecutor {
        let artifact = Artifact {
            repo_path: view.repo_path.clone(),
            branch: "main".to_string(),
            commit_sha: view.commit_sha.clone(),
        };
        let workspace = create_temporary_workspace(&artifact).unwrap();
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

        assert_eq!(
            response,
            FileToolResponse::Failed {
                reason: "Use a relative path from the allowed target list: main.py".to_string(),
            }
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

        assert_eq!(
            response,
            FileToolResponse::Failed {
                reason: "Use a relative path from the allowed target list: main.py".to_string(),
            }
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

    #[test]
    fn parse_replace_text_with_placeholder_old_is_rejected() {
        let result =
            parse_tool_request(r#"{"tool":"replace_text","path":"f.txt","old":"...","new":"x"}"#);
        assert!(
            result.is_err(),
            "replace_text with placeholder old must be rejected; got {result:?}"
        );
        assert!(
            result.unwrap_err().contains("placeholder"),
            "error must mention placeholder"
        );
    }

    #[test]
    fn parse_replace_text_with_placeholder_new_is_rejected() {
        let result =
            parse_tool_request(r#"{"tool":"replace_text","path":"f.txt","old":"x","new":"..."}"#);
        assert!(
            result.is_err(),
            "replace_text with placeholder new must be rejected; got {result:?}"
        );
    }

    #[test]
    fn parse_write_file_with_placeholder_content_is_rejected() {
        let result =
            parse_tool_request(r#"{"tool":"write_file","path":"output.txt","content":"..."}"#);
        assert!(
            result.is_err(),
            "write_file with placeholder content must be rejected; got {result:?}"
        );
    }

    #[test]
    fn parse_write_file_with_dollar_placeholder_path_is_rejected() {
        let result = parse_tool_request(
            r#"{"tool":"write_file","path":"$TARGET_FILE","content":"real content"}"#,
        );
        assert!(
            result.is_err(),
            "write_file with $TARGET_FILE path must be rejected; got {result:?}"
        );
        assert!(
            result.unwrap_err().contains("placeholder"),
            "error must mention placeholder"
        );
    }

    #[test]
    fn parse_write_file_with_dollar_placeholder_content_is_rejected() {
        let result = parse_tool_request(
            r#"{"tool":"write_file","path":"real.txt","content":"$FILE_CONTENT"}"#,
        );
        assert!(
            result.is_err(),
            "write_file with $FILE_CONTENT content must be rejected; got {result:?}"
        );
        assert!(
            result.unwrap_err().contains("placeholder"),
            "error must mention placeholder"
        );
    }

    #[test]
    fn parse_write_file_with_real_content_is_accepted() {
        let result = parse_tool_request(
            r#"{"tool":"write_file","path":"output.txt","content":"hello world"}"#,
        );
        assert!(
            result.is_ok(),
            "write_file with real content must parse successfully; got {result:?}"
        );
    }

    #[test]
    fn parse_replace_text_with_real_values_is_accepted() {
        let result = parse_tool_request(
            r#"{"tool":"replace_text","path":"f.txt","old":"hello","new":"goodbye"}"#,
        );
        assert!(
            result.is_ok(),
            "replace_text with real values must parse successfully; got {result:?}"
        );
    }

    // ── trailing-whitespace robustness ───────────────────────────────────────

    #[test]
    fn parse_tool_request_with_trailing_newline() {
        let result = parse_tool_request("{\"tool\":\"list_files\"}\n");
        assert!(
            result.is_ok(),
            "trailing newline must not cause parse failure; got {result:?}"
        );
        assert_eq!(result.unwrap(), FileToolRequest::ListFiles);
    }

    #[test]
    fn parse_tool_request_with_trailing_spaces_and_tabs() {
        let result = parse_tool_request("{\"tool\":\"list_files\"}  \t  ");
        assert!(
            result.is_ok(),
            "trailing spaces/tabs must not cause parse failure; got {result:?}"
        );
        assert_eq!(result.unwrap(), FileToolRequest::ListFiles);
    }

    #[test]
    fn parse_tool_request_with_trailing_newlines_write_file() {
        let result = parse_tool_request(
            "{\"tool\":\"write_file\",\"path\":\".gitignore\",\"content\":\"*.log\\n\"}\n\n",
        );
        assert!(
            result.is_ok(),
            "write_file with trailing newlines must parse; got {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            FileToolRequest::WriteFile {
                path: ".gitignore".to_owned(),
                content: "*.log\n".to_owned(),
            }
        );
    }

    #[test]
    fn parse_tool_request_with_leading_whitespace() {
        let result = parse_tool_request("  \n{\"tool\":\"list_files\"}");
        assert!(
            result.is_ok(),
            "leading whitespace must not cause parse failure; got {result:?}"
        );
    }
}
