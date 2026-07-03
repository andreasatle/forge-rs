//! Project-level adapter seam.
//!
//! [`ProjectAdapter`] is the hook through which project-specific configuration
//! is injected into the runtime. The seam exposes role prompt policy and
//! target-file projection; future variants can add export config, validation
//! config, or integration movement without changing the runtime wiring.

pub mod coding;
pub mod coding_tdd;
pub mod default;
pub mod yaml;
pub mod yaml_config;

pub use coding::CodingProjectAdapter;
pub use coding_tdd::CodingTddProjectAdapter;
pub use default::DefaultProjectAdapter;
pub use yaml::YamlProjectAdapter;
pub use yaml_config::{ProjectAdapterConfig, RolePromptConfig, RolePromptsConfig};

use crate::artifacts::ArtifactRead;
use crate::machines::deliberation::DeliberationRole;
use crate::roles::{RolePolicy, TargetView};

/// Provides project-specific configuration to the Forge runtime.
///
/// Implement this trait to customise the role prompt policy, target-file
/// projection, and (in future) other project-level knobs without touching the
/// runtime directly.
pub trait ProjectAdapter {
    /// Return the per-role system prompt policy for this project.
    fn role_policy(&self) -> RolePolicy;

    /// Build prompt-context views for the given target files.
    ///
    /// The adapter decides the representation for each file: full text, an
    /// excerpt, a summary, an absent marker, a binary notice, or any other
    /// project-specific projection. The runner only renders the returned views
    /// and never inspects the artifact directly for target-state content.
    ///
    /// `budget` is the maximum number of bytes the framework is willing to
    /// include per file in the text representation. Adapters may respect it or
    /// apply their own limit.
    ///
    /// The default projects each target as full text (subject to `budget`)
    /// via [`build_file_text_target_views`]. Adapters override this to supply
    /// a project-specific representation.
    fn build_target_views(
        &self,
        artifact_view: &dyn ArtifactRead,
        targets: &[String],
        _role: &DeliberationRole,
        budget: usize,
    ) -> Vec<TargetView> {
        build_file_text_target_views(artifact_view, targets, budget)
    }

    /// Returns artifact file names whose contents should be included as
    /// ambient context in the deliberation objective.
    ///
    /// The framework reads each named file from the artifact (if present) and
    /// prepends its content to the prompt. The default returns no context
    /// files; adapters override this to expose project-specific context.
    fn context_file_names(&self) -> Vec<String> {
        vec![]
    }
}

/// Shared file-text projection used by both built-in adapters.
///
/// Reads each target from `artifact_view` and produces a [`TargetView`] with:
/// - `FullText` when the file fits within `budget` bytes,
/// - `TooLarge` when it exceeds `budget`,
/// - `Absent` when the file does not exist,
/// - `Error` for any other read failure (binary / non-UTF-8 is described safely).
pub(crate) fn build_file_text_target_views(
    artifact_view: &dyn ArtifactRead,
    targets: &[String],
    budget: usize,
) -> Vec<TargetView> {
    use crate::artifacts::ArtifactError;
    use crate::roles::TargetViewKind;

    if targets.is_empty() {
        return vec![];
    }

    let listed_paths = artifact_view.list_files().ok();

    targets
        .iter()
        .map(|target| match artifact_view.read_file(target) {
            Ok(content) if content.len() <= budget => TargetView {
                id: target.clone(),
                exists: true,
                kind: TargetViewKind::FullText,
                representation: content,
            },
            Ok(content) => TargetView {
                id: target.clone(),
                exists: true,
                kind: TargetViewKind::TooLarge,
                representation: format!(
                    "too large to include safely ({} bytes; limit {budget} bytes)",
                    content.len()
                ),
            },
            Err(ArtifactError::FileNotFound) => TargetView {
                id: target.clone(),
                exists: false,
                kind: TargetViewKind::Absent,
                representation: String::new(),
            },
            Err(error) => {
                let exists = listed_paths
                    .as_deref()
                    .map(|paths| {
                        paths
                            .iter()
                            .any(|path| path.to_string_lossy().as_ref() == target)
                    })
                    .unwrap_or(false);
                TargetView {
                    id: target.clone(),
                    exists,
                    kind: TargetViewKind::Error,
                    representation: safe_target_error(error),
                }
            }
        })
        .collect()
}

fn safe_target_error(error: crate::artifacts::ArtifactError) -> String {
    use crate::artifacts::ArtifactError;
    match error {
        ArtifactError::Encoding => {
            "binary or non-UTF-8 file cannot be represented as text".to_string()
        }
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactError;
    use crate::project::{CodingProjectAdapter, DefaultProjectAdapter};

    // ── safe_target_error ────────────────────────────────────────────────────

    #[test]
    fn encoding_error_produces_user_friendly_message() {
        let msg = safe_target_error(ArtifactError::Encoding);
        assert!(
            msg.contains("binary") || msg.contains("non-UTF-8"),
            "encoding error must describe binary/encoding issue; got: {msg}"
        );
        assert!(
            !msg.contains("utf-8") && !msg.contains("utf8"),
            "encoding error must not leak raw error text; got: {msg}"
        );
    }

    #[test]
    fn other_errors_are_passed_through_unchanged() {
        let cases = [
            ArtifactError::IoError("disk full".to_string()),
            ArtifactError::FileNotFound,
        ];
        for error in cases {
            let expected = error.to_string();
            let msg = safe_target_error(error);
            assert_eq!(msg, expected);
        }
    }

    // ── ProjectAdapter::context_file_names ───────────────────────────────────

    #[test]
    fn default_adapter_context_file_names_is_empty() {
        let names = DefaultProjectAdapter.context_file_names();
        assert!(
            names.is_empty(),
            "DefaultProjectAdapter must return no context file names; got: {names:?}"
        );
    }

    #[test]
    fn coding_adapter_context_file_names_includes_readme() {
        let names = CodingProjectAdapter.context_file_names();
        assert!(
            names.contains(&"README.md".to_string()),
            "CodingProjectAdapter must include README.md as a context file; got: {names:?}"
        );
    }
}
