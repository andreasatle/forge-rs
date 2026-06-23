use std::path::PathBuf;
use std::process::Command;

use super::file_ops::{ArtifactError, validate_relative_path};

/// A committed version stored in a bare Git repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    /// Path to the bare repository that stores the artifact's commits.
    pub repo_path: PathBuf,
    /// Logical branch associated with the artifact.
    pub branch: String,
    /// Exact commit containing this artifact version.
    pub commit_sha: String,
}

/// A read-only view of a specific artifact version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactView {
    /// Path to the bare repository backing this view.
    pub repo_path: PathBuf,
    /// Exact commit exposed by this view.
    pub commit_sha: String,
}

impl ArtifactView {
    /// Returns all file paths in this commit, relative to the repository root.
    ///
    /// Paths are sorted deterministically. The `.git` entry never appears
    /// because bare repositories hold no working tree.
    pub fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        let output = Command::new("git")
            .args(["ls-tree", "-r", "--name-only", &self.commit_sha])
            .current_dir(&self.repo_path)
            .output()
            .map_err(|e| ArtifactError::IoError(e.to_string()))?;

        if !output.status.success() {
            return Err(ArtifactError::IoError(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }

        let stdout =
            String::from_utf8(output.stdout).map_err(|e| ArtifactError::IoError(e.to_string()))?;

        let mut paths: Vec<PathBuf> = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        paths.sort();
        Ok(paths)
    }

    /// Reads the contents of `path` at the pinned commit.
    ///
    /// Returns `ArtifactError::PathOutsideWorkspace` for absolute paths or
    /// parent traversals, and `ArtifactError::FileNotFound` when the path
    /// does not exist in the commit.
    pub fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        validate_relative_path(path)?;

        let object = format!("{}:{}", self.commit_sha, path);
        let output = Command::new("git")
            .args(["show", &object])
            .current_dir(&self.repo_path)
            .output()
            .map_err(|e| ArtifactError::IoError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("does not exist in")
                || stderr.contains("not a valid object name")
                || stderr.contains("exists on disk")
            {
                return Err(ArtifactError::FileNotFound);
            }
            return Err(ArtifactError::IoError(stderr.trim().to_owned()));
        }

        String::from_utf8(output.stdout).map_err(|e| ArtifactError::IoError(e.to_string()))
    }
}
