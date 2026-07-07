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
    /// Optional additional paths that may be read, but never written, even
    /// when `allowed_paths` restricts primary access.
    ///
    /// Used to expose required validation target files (e.g. tests) to
    /// read-only reviewer roles without granting them write access or
    /// widening the Producer's own target files.
    pub additional_read_only_paths: Option<Vec<String>>,
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
            additional_read_only_paths: None,
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

    fn validate_path_allowed(&self, path: &str, read_only: bool) -> Result<(), String> {
        validate_relative_path(path).map_err(|e| match (&self.policy.allowed_paths, e) {
            (Some(allowed), ArtifactError::PathOutsideWorkspace) => {
                allowed_target_guidance(allowed)
            }
            (_, e) => e.to_string(),
        })?;
        if let Some(allowed) = &self.policy.allowed_paths
            && !allowed.iter().any(|target| target == path)
        {
            let extra_allowed = read_only
                && self
                    .policy
                    .additional_read_only_paths
                    .as_ref()
                    .is_some_and(|paths| paths.iter().any(|target| target == path));
            if !extra_allowed {
                return Err(allowed_target_guidance(allowed));
            }
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
                if let Err(reason) = self.validate_path_allowed(&path, true) {
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
                if let Err(reason) = self.validate_path_allowed(&path, false) {
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
                if let Err(reason) = self.validate_path_allowed(&path, false) {
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
                if let Err(reason) = self.validate_path_allowed(&path, false) {
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
    let mut req: FileToolRequest = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if let FileToolRequest::WriteFile { content, .. } = &mut req {
        *content = unescape_literal_newlines(content);
    }
    if has_placeholder_fields(&req) {
        return Err("tool request contains placeholder values".to_string());
    }
    Ok(req)
}

/// Returns `true` when `json` is a JSON object carrying a `"tool"` key,
/// indicating the model intended to issue a tool request even if
/// [`parse_tool_request`] went on to reject it as malformed.
///
/// Used to decide whether a failed tool-request parse should be surfaced to
/// the model as a malformed tool call, rather than silently falling through
/// to a misleading "role response could not be parsed" error.
pub fn looks_like_tool_request(json: &str) -> bool {
    let Some(json) = extract_json_object(json.trim()) else {
        return false;
    };
    matches!(
        serde_json::from_str::<serde_json::Value>(json),
        Ok(serde_json::Value::Object(map)) if map.contains_key("tool")
    )
}

/// Replaces literal two-character `\n` sequences with real newline characters.
///
/// Models frequently double-escape newlines when emitting JSON tool calls,
/// producing a backslash followed by `n` instead of an actual newline byte.
/// `write_file` content should always land on disk with real newlines, so
/// this normalization runs once, right after JSON parsing.
fn unescape_literal_newlines(s: &str) -> String {
    s.replace("\\n", "\n")
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
#[path = "file_tests.rs"]
mod tests;
