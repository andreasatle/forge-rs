//! Objective enrichment for deliberation runs.

use crate::artifacts::ArtifactView;

use crate::node_runner::types::NodeRunRequest;

/// Returns the objective string, optionally prefixed with artifact file context.
pub(crate) fn enrich_objective(
    request: &NodeRunRequest,
    requires_tests: bool,
    context_file_names: &[String],
) -> String {
    let testing_context = if requires_tests {
        Some(
            "Testing requirement: project validation includes a test command. Code changes require corresponding tests, and plans for code changes must include at least one test-related target.".to_string(),
        )
    } else {
        None
    };
    let Some(view) = &request.artifact_view else {
        return match testing_context {
            Some(context) => format!("{context}\n\nObjective: {}", request.objective),
            None => request.objective.clone(),
        };
    };
    let context = build_artifact_context(view, context_file_names);
    if context.is_empty() {
        return match testing_context {
            Some(testing_context) => {
                format!("{testing_context}\n\nObjective: {}", request.objective)
            }
            None => request.objective.clone(),
        };
    }
    match testing_context {
        Some(testing_context) => {
            format!(
                "{context}\n\n{testing_context}\n\nObjective: {}",
                request.objective
            )
        }
        None => format!("{context}\n\nObjective: {}", request.objective),
    }
}

/// Builds a short context string from a read-only artifact view.
///
/// Lists all files under a heading that signals they already exist and must
/// not be recreated unless the objective explicitly names them. Then includes
/// the content of each file named in `context_file_names` if present.
///
/// Returns an empty string when the view has no files or when git fails.
pub(crate) fn build_artifact_context(view: &ArtifactView, context_file_names: &[String]) -> String {
    let files = match view.list_files() {
        Ok(f) if !f.is_empty() => f,
        _ => return String::new(),
    };
    let mut parts = Vec::new();
    let listing: Vec<String> = files.iter().map(|p| format!("  {}", p.display())).collect();
    parts.push(format!(
        "Existing project files (already initialized — do not create tasks to recreate \
         or reinitialize these files unless the objective explicitly names them as targets):\n{}",
        listing.join("\n")
    ));
    for name in context_file_names {
        if let Ok(content) = view.read_file(name) {
            parts.push(format!("{name}:\n{content}"));
        }
    }
    parts.join("\n\n")
}
