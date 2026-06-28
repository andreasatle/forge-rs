//! Project-level adapter seam.
//!
//! [`ProjectAdapter`] is the hook through which project-specific configuration
//! is injected into the runtime. The seam exposes role prompt policy and
//! target-file projection; future variants can add export config, validation
//! config, or integration movement without changing the runtime wiring.

pub mod coding;
pub mod default;

pub use coding::CodingProjectAdapter;
pub use default::DefaultProjectAdapter;

use crate::artifacts::ArtifactRead;
use crate::machines::deliberation::state::DeliberationRole;
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
    fn build_target_views(
        &self,
        artifact_view: &dyn ArtifactRead,
        targets: &[String],
        role: &DeliberationRole,
        budget: usize,
    ) -> Vec<TargetView>;
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
    let message = error.to_string();
    if message.contains("utf-8") || message.contains("utf8") {
        "binary or non-UTF-8 file cannot be represented as text".to_string()
    } else {
        message
    }
}
