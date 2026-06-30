//! DeliberationMachine input types.
//!
//! These are the caller-facing shapes that enter the machine at construction
//! time. They are not durable machine state; they travel inside state variants
//! unchanged for the lifetime of a run.

/// Prompt context that travels beside the canonical objective.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeliberationContext {
    /// Structured target files the pipeline should use for file-tool policy
    /// and prompt context.
    pub target_files: Vec<String>,
    /// Adapter-level testing requirement for code changes, when present.
    pub testing_requirement: Option<String>,
    /// Read-only artifact context made visible to roles.
    pub artifact: Option<ArtifactContext>,
}

/// Read-only artifact context captured before the deliberation run.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactContext {
    /// Existing files in the artifact.
    pub files: Vec<String>,
    /// Selected file contents included as prompt context.
    pub selected_files: Vec<SelectedFileContent>,
}

/// Content for a selected artifact file.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedFileContent {
    /// Artifact-relative path.
    pub path: String,
    /// File content at the captured artifact commit.
    pub content: String,
}

/// The input submitted to the deliberation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliberationRequest {
    /// The canonical objective the pipeline should address.
    pub objective: String,
    /// Structured prompt/tooling context for this run.
    pub context: DeliberationContext,
    /// Maximum number of revision loops allowed.
    ///
    /// `0` means no revisions: the first Referee rejection fails immediately.
    pub max_revisions: usize,
}
