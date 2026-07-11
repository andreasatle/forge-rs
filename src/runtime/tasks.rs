use std::error::Error;

use crate::artifacts::{Artifact, ArtifactError, ArtifactView, manifest_tasks};
use crate::config::ForgeConfig;

const MANIFEST_PATH: &str = ".forge/tasks.json";

/// Print each task recorded in `.forge/tasks.json` at the artifact's current commit.
pub fn run_tasks(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let artifact = super::load_or_create_artifact(&config.artifact, None)?;
    let output = tasks_contents(&artifact)?;
    print!("{output}");
    Ok(())
}

fn tasks_contents(artifact: &Artifact) -> Result<String, Box<dyn Error>> {
    let view = ArtifactView {
        repo_path: artifact.repo_path.clone(),
        commit_sha: artifact.commit_sha.clone(),
    };

    let contents = match view.read_file(MANIFEST_PATH) {
        Ok(contents) => Some(contents),
        Err(ArtifactError::FileNotFound) => None,
        Err(e) => return Err(e.into()),
    };

    let tasks = manifest_tasks(contents.as_deref())?;
    if tasks.is_empty() {
        return Ok("No tasks recorded.\n".to_string());
    }

    let mut out = String::new();
    for task in &tasks {
        out.push_str(&format!("id       : {}\n", task.id));
        out.push_str(&format!(
            "name     : {}\n",
            task.name.as_deref().unwrap_or("(none)")
        ));
        out.push_str(&format!("objective: {}\n", task.objective));
        out.push_str(&format!(
            "team     : {}\n",
            task.team.as_deref().unwrap_or("(none)")
        ));
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{WorkspaceFactory, WorkspaceFileOps, integrate};
    use crate::config::ArtifactConfig;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-tasks-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    fn fresh_artifact(base: &std::path::Path) -> Artifact {
        let repo_path = base.join("artifact.git");
        let config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };
        crate::runtime::load_or_create_artifact(&config, None).unwrap()
    }

    #[test]
    fn tasks_reports_none_when_manifest_missing() {
        let base = temp_path("missing");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let artifact = fresh_artifact(&base);
        let output = tasks_contents(&artifact).unwrap();

        assert_eq!(
            output, "No tasks recorded.\n",
            "a fresh artifact with no manifest must report no tasks, got: {output}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tasks_lists_recorded_task_fields() {
        let base = temp_path("list");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let artifact = fresh_artifact(&base);

        let manifest_json = r#"{
            "schema_version": 1,
            "tasks": [
                {
                    "id": "task-1",
                    "objective": "Implement the widget",
                    "targets": ["widget.rs"],
                    "commit": "deadbeef",
                    "completed_at": "2024-01-01T00:00:00Z",
                    "team": "core",
                    "name": "widget"
                }
            ]
        }"#;

        let workspace_path = base.join("workspace");
        let mut workspace = WorkspaceFactory::new(&artifact).create_workspace(workspace_path);
        workspace
            .write_file(".forge/tasks.json", manifest_json)
            .unwrap();
        let integrated = integrate(&artifact, &workspace).unwrap();

        let output = tasks_contents(&integrated).unwrap();

        assert!(output.contains("id       : task-1"), "got: {output}");
        assert!(output.contains("name     : widget"), "got: {output}");
        assert!(
            output.contains("objective: Implement the widget"),
            "got: {output}"
        );
        assert!(output.contains("team     : core"), "got: {output}");

        let _ = std::fs::remove_dir_all(&base);
    }
}
