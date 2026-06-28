//! Adapter-produced projection of a target file for prompt injection.

/// How the adapter chose to represent this target file.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TargetViewKind {
    /// File content is included as plain text.
    FullText,
    /// File does not exist in the artifact.
    Absent,
    /// File exists but is too large to include.
    TooLarge,
    /// File could not be read (e.g. binary, permission error).
    Error,
}

/// Adapter-produced view of one target file for inclusion in the role prompt.
///
/// The adapter decides what to put here; the [`RoleRunner`] only renders it.
///
/// [`RoleRunner`]: crate::roles::RoleRunner
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TargetView {
    /// The target path as declared in the task.
    pub id: String,
    /// Whether the file exists in the artifact at the time of viewing.
    pub exists: bool,
    /// How the content is represented.
    pub kind: TargetViewKind,
    /// The representation payload.
    ///
    /// - `FullText`: raw file content.
    /// - `Absent`: empty string (unused).
    /// - `TooLarge`: human-readable size message, e.g. "too large to include safely (N bytes; limit M bytes)".
    /// - `Error`: sanitised error summary.
    pub representation: String,
}
