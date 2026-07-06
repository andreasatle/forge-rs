//! Per-file API summary extraction for planning-node prompts.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::artifacts::{Artifact, ArtifactView, WorkspaceFactory};
use crate::validation::CommandSpec;

/// Runs `command` against each of `files` inside a temporary checkout of
/// `view`, joining the non-empty outputs into a single listing keyed by file
/// path. Returns `None` when the checkout fails or no file yields output.
pub(crate) fn build_api_summary(
    view: &ArtifactView,
    files: &[PathBuf],
    command: &CommandSpec,
) -> Option<String> {
    let artifact = Artifact {
        repo_path: view.repo_path.clone(),
        branch: String::new(),
        commit_sha: view.commit_sha.clone(),
    };
    let workspace = WorkspaceFactory::new(&artifact)
        .create_temporary_workspace()
        .ok()?;

    let sections: Vec<String> = files
        .iter()
        .filter_map(|file| summarize_file(workspace.path(), file, command))
        .collect();

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn summarize_file(workspace_path: &Path, file: &Path, command: &CommandSpec) -> Option<String> {
    let display_path = file.to_string_lossy();
    let output = Command::new(&command.program)
        .args(&command.args)
        .arg(display_path.as_ref())
        .current_dir(workspace_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("# {display_path}\n{trimmed}"))
}

#[cfg(test)]
#[path = "api_summary_tests.rs"]
mod tests;
