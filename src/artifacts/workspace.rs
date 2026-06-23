use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::Artifact;

/// A temporary mutable, non-bare checkout derived from an artifact version.
///
/// When `cleanup` is `Some`, the temporary directory is deleted automatically
/// on drop. When `cleanup` is `None`, the caller owns the directory.
#[derive(Debug)]
pub struct Workspace {
    path: PathBuf,
    _cleanup: Option<TempDir>,
    /// Commit from which the workspace was created.
    pub base_commit: String,
}

impl Workspace {
    /// Creates a workspace descriptor rooted at `path` with no automatic cleanup.
    ///
    /// The caller is responsible for the lifetime of the directory.
    pub fn at_path(path: PathBuf, base_commit: String) -> Self {
        Self {
            path,
            _cleanup: None,
            base_commit,
        }
    }

    /// Returns the root path of the workspace.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Creates a mutable clone checked out at the artifact's exact commit.
///
/// The directory at `workspace_path` is NOT deleted on drop. The caller
/// is responsible for any cleanup.
pub fn create_workspace(artifact: &Artifact, workspace_path: PathBuf) -> Workspace {
    git_clone_artifact(artifact, &workspace_path)
        .expect("failed to create workspace via git clone/checkout");
    Workspace::at_path(workspace_path, artifact.commit_sha.clone())
}

/// Creates a mutable clone in a freshly allocated temporary directory.
///
/// The directory is deleted automatically when the returned [`Workspace`] is
/// dropped. Even if apply, validation, or integration fails, the directory is
/// removed as long as the `Workspace` value is dropped.
pub fn create_temporary_workspace(artifact: &Artifact) -> Result<Workspace, Box<dyn Error>> {
    let temp = TempDir::new()?;
    git_clone_artifact(artifact, temp.path())?;
    Ok(Workspace {
        path: temp.path().to_path_buf(),
        _cleanup: Some(temp),
        base_commit: artifact.commit_sha.clone(),
    })
}

fn git_clone_artifact(artifact: &Artifact, workspace_path: &Path) -> Result<(), Box<dyn Error>> {
    let clone = Command::new("git")
        .args(["clone", "--quiet", "--no-checkout"])
        .arg(&artifact.repo_path)
        .arg(workspace_path)
        .output()
        .map_err(|e| format!("failed to run git clone: {e}"))?;
    if !clone.status.success() {
        return Err(format!(
            "git clone failed while creating workspace: {}",
            String::from_utf8_lossy(&clone.stderr).trim()
        )
        .into());
    }

    let checkout = Command::new("git")
        .args(["checkout", "--quiet", "--detach"])
        .arg(&artifact.commit_sha)
        .current_dir(workspace_path)
        .output()
        .map_err(|e| format!("failed to run git checkout: {e}"))?;
    if !checkout.status.success() {
        return Err(format!(
            "git checkout failed while creating workspace: {}",
            String::from_utf8_lossy(&checkout.stderr).trim()
        )
        .into());
    }

    Ok(())
}
