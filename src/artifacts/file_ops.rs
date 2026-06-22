use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::Workspace;

/// Errors produced by workspace file operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtifactError {
    /// The requested workspace file does not exist.
    FileNotFound,
    /// The requested replacement text does not occur in the file.
    ReplaceTargetMissing,
    /// The requested replacement text occurs more than once in the file.
    ReplaceTargetAmbiguous,
    /// A filesystem or text-decoding operation failed.
    IoError(String),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileNotFound => formatter.write_str("file not found"),
            Self::ReplaceTargetMissing => formatter.write_str("replacement target not found"),
            Self::ReplaceTargetAmbiguous => {
                formatter.write_str("replacement target occurs more than once")
            }
            Self::IoError(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// Fundamental text-file operations over an artifact workspace.
pub trait WorkspaceFileOps {
    /// Lists artifact file paths relative to the workspace root.
    fn list_files(&self) -> Vec<PathBuf>;
    /// Reads a workspace file as UTF-8 text.
    fn read_file(&self, path: &str) -> Result<String, ArtifactError>;
    /// Creates or overwrites a workspace file and any missing parent directories.
    fn write_file(&mut self, path: &str, content: &str) -> Result<(), ArtifactError>;
    /// Replaces the sole exact occurrence of `old` in a workspace file.
    fn replace_text(&mut self, path: &str, old: &str, new: &str) -> Result<(), ArtifactError>;
    /// Deletes an existing workspace file.
    fn delete_file(&mut self, path: &str) -> Result<(), ArtifactError>;
}

impl WorkspaceFileOps for Workspace {
    fn list_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_files(&self.path, &self.path, &mut files);
        files.sort();
        files
    }

    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        fs::read_to_string(self.path.join(path)).map_err(map_file_error)
    }

    fn write_file(&mut self, path: &str, content: &str) -> Result<(), ArtifactError> {
        let destination = self.path.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(map_io_error)?;
        }
        fs::write(destination, content).map_err(map_io_error)
    }

    fn replace_text(&mut self, path: &str, old: &str, new: &str) -> Result<(), ArtifactError> {
        let content = self.read_file(path)?;
        let mut occurrences = content.match_indices(old);
        let Some((start, _)) = occurrences.next() else {
            return Err(ArtifactError::ReplaceTargetMissing);
        };
        if occurrences.next().is_some() {
            return Err(ArtifactError::ReplaceTargetAmbiguous);
        }

        let mut updated = String::with_capacity(content.len() - old.len() + new.len());
        updated.push_str(&content[..start]);
        updated.push_str(new);
        updated.push_str(&content[start + old.len()..]);
        self.write_file(path, &updated)
    }

    fn delete_file(&mut self, path: &str) -> Result<(), ArtifactError> {
        fs::remove_file(self.path.join(path)).map_err(map_file_error)
    }
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path == root.join(".git") {
            continue;
        }
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => collect_files(root, &path, files),
            Ok(file_type) if file_type.is_file() => {
                if let Ok(relative) = path.strip_prefix(root) {
                    files.push(relative.to_path_buf());
                }
            }
            _ => {}
        }
    }
}

fn map_file_error(error: io::Error) -> ArtifactError {
    if error.kind() == io::ErrorKind::NotFound {
        ArtifactError::FileNotFound
    } else {
        map_io_error(error)
    }
}

fn map_io_error(error: io::Error) -> ArtifactError {
    ArtifactError::IoError(error.to_string())
}
