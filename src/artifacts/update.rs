use std::fs;

use super::Workspace;

/// Complete file replacements to apply to a workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactUpdate {
    /// Files to create or replace.
    pub files: Vec<UpdatedFile>,
}

/// A file path and its complete new contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdatedFile {
    /// Path relative to the workspace root.
    pub path: String,
    /// Complete contents to write.
    pub content: String,
}

/// Applies complete file replacements to a workspace.
pub fn apply_update(workspace: &Workspace, update: &ArtifactUpdate) {
    for file in &update.files {
        let destination = workspace.path.join(&file.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|error| {
                panic!(
                    "failed to create parent directories for {}: {error}",
                    file.path
                )
            });
        }
        fs::write(&destination, &file.content)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", file.path));
    }
}
