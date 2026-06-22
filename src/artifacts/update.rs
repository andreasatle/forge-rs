use super::Workspace;
use super::file_ops::{ArtifactError, WorkspaceFileOps};

/// A single file change to apply to a workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileChange {
    /// Creates or overwrites a file with the given content.
    Write {
        /// Path relative to the workspace root.
        path: String,
        /// Complete file contents to write.
        content: String,
    },
    /// Replaces the sole exact occurrence of `old` with `new` in a file.
    Replace {
        /// Path relative to the workspace root.
        path: String,
        /// Text to find (must occur exactly once).
        old: String,
        /// Replacement text.
        new: String,
    },
    /// Deletes an existing file.
    Delete {
        /// Path relative to the workspace root.
        path: String,
    },
}

/// An ordered sequence of file changes to apply to a workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactUpdate {
    /// The changes to apply, in order.
    pub changes: Vec<FileChange>,
}

impl ArtifactUpdate {
    /// Applies all changes in order, stopping at the first error.
    pub fn apply(&self, workspace: &mut Workspace) -> Result<(), ArtifactError> {
        for change in &self.changes {
            match change {
                FileChange::Write { path, content } => {
                    workspace.write_file(path, content)?;
                }
                FileChange::Replace { path, old, new } => {
                    workspace.replace_text(path, old, new)?;
                }
                FileChange::Delete { path } => {
                    workspace.delete_file(path)?;
                }
            }
        }
        Ok(())
    }
}
