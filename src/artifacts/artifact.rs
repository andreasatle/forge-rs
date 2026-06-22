use std::path::PathBuf;

/// A committed version of a Git-backed artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    /// Path to the repository that stores the artifact's commits.
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
