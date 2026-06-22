use std::path::PathBuf;
use std::process::Command;

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

/// A read-only reference to an artifact version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactView {
    /// Exact commit exposed by this view.
    pub commit_sha: String,
}

pub(crate) fn assert_bare_repository(artifact: &Artifact) {
    let output = Command::new("git")
        .args(["rev-parse", "--is-bare-repository"])
        .current_dir(&artifact.repo_path)
        .output()
        .expect("failed to inspect artifact repository");
    assert!(
        output.status.success(),
        "artifact repository is not a Git repository: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "true",
        "artifact repository must be bare"
    );
}
