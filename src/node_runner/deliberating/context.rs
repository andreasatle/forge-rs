//! Structured context capture for deliberation runs.

use std::sync::Arc;

use crate::artifacts::ArtifactView;
use crate::machines::deliberation::{ArtifactContext, DeliberationContext, SelectedFileContent};
use crate::machines::scheduler::NodeKind;
use crate::node_runner::TestTargetsFn;
use crate::node_runner::types::NodeRunRequest;
use crate::validation::CommandSpec;

use super::api_summary::build_api_summary;

pub(crate) const TESTING_REQUIREMENT: &str = "Testing requirement: project validation includes a test command. Code changes require corresponding tests, and plans for code changes must include at least one test-related target.";

/// Adapter-derived inputs threaded into deliberation context construction,
/// grouped so call sites don't accumulate unrelated parameters.
pub(crate) struct DeliberationContextConfig<'a> {
    /// Probed with a sentinel code file to determine whether the project
    /// adapter requires tests.
    pub(crate) required_test_targets_fn: &'a Arc<TestTargetsFn>,
    pub(crate) context_file_names: &'a [String],
    /// Language plugin's per-file API summary command, when configured. Run
    /// for `Decomposition` and `Plan` nodes to surface existing API shape to
    /// the planner.
    pub(crate) api_summary_command: Option<&'a CommandSpec>,
    /// The configured northstar text, when present. Surfaced only to
    /// `Decomposition` nodes alongside the API summary.
    pub(crate) northstar: Option<&'a str>,
}

/// Returns structured context for a deliberation run.
pub(crate) fn build_deliberation_context(
    request: &NodeRunRequest,
    config: &DeliberationContextConfig,
) -> DeliberationContext {
    let requires_tests = !(config.required_test_targets_fn)(&["_probe_.rs".to_string()]).is_empty();
    let testing_requirement = if requires_tests {
        Some(TESTING_REQUIREMENT.to_string())
    } else {
        None
    };

    let northstar = matches!(request.kind, NodeKind::Decomposition)
        .then(|| config.northstar)
        .flatten()
        .map(str::to_string);

    DeliberationContext {
        target_files: request.target_files.clone(),
        testing_requirement,
        artifact: request.artifact_view.as_ref().and_then(|view| {
            build_artifact_context(
                view,
                config.context_file_names,
                &request.kind,
                config.api_summary_command,
            )
        }),
        northstar,
    }
}

/// Builds structured context from a read-only artifact view.
///
/// Returns `None` when the view has no files or when git fails.
pub(crate) fn build_artifact_context(
    view: &ArtifactView,
    context_file_names: &[String],
    node_kind: &NodeKind,
    api_summary_command: Option<&CommandSpec>,
) -> Option<ArtifactContext> {
    let files = match view.list_files() {
        Ok(f) if !f.is_empty() => f,
        _ => return None,
    };
    let selected_files = context_file_names
        .iter()
        .filter_map(|name| {
            view.read_file(name)
                .ok()
                .map(|content| SelectedFileContent {
                    path: name.clone(),
                    content,
                })
        })
        .collect();

    let api_summary = api_summary_command
        .filter(|_| matches!(node_kind, NodeKind::Decomposition | NodeKind::Plan))
        .and_then(|command| build_api_summary(view, &files, command));

    Some(ArtifactContext {
        files: files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        selected_files,
        api_summary,
    })
}
