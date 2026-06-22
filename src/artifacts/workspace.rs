use std::path::PathBuf;
use std::process::Command;

use super::Artifact;

/// A temporary mutable checkout derived from an artifact version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    /// Root path of the workspace checkout.
    pub path: PathBuf,
    /// Commit from which the workspace was created.
    pub base_commit: String,
}

/// Creates a mutable clone checked out at the artifact's exact commit.
pub fn create_workspace(artifact: &Artifact, workspace_path: PathBuf) -> Workspace {
    let clone = Command::new("git")
        .args(["clone", "--quiet", "--no-checkout"])
        .arg(&artifact.repo_path)
        .arg(&workspace_path)
        .output()
        .expect("failed to run git clone while creating workspace");
    assert!(
        clone.status.success(),
        "git clone failed while creating workspace: {}",
        String::from_utf8_lossy(&clone.stderr).trim()
    );

    let checkout = Command::new("git")
        .args(["checkout", "--quiet", "--detach"])
        .arg(&artifact.commit_sha)
        .current_dir(&workspace_path)
        .output()
        .expect("failed to run git checkout while creating workspace");
    assert!(
        checkout.status.success(),
        "git checkout failed while creating workspace: {}",
        String::from_utf8_lossy(&checkout.stderr).trim()
    );

    Workspace {
        path: workspace_path,
        base_commit: artifact.commit_sha.clone(),
    }
}
