use std::error::Error;

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::ForgeConfig;

/// Display the current artifact contents from the config's artifact repo.
pub fn run_show(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let artifact = super::load_or_create_artifact(&config.artifact)?;
    let output = artifact_contents(&artifact)?;
    print!("{output}");
    Ok(())
}

fn artifact_contents(artifact: &Artifact) -> Result<String, Box<dyn Error>> {
    let view = ArtifactView {
        repo_path: artifact.repo_path.clone(),
        commit_sha: artifact.commit_sha.clone(),
    };

    let short_sha = &artifact.commit_sha[..artifact.commit_sha.len().min(7)];
    let mut out = format!("Commit      : {short_sha}\n");

    let files = view.list_files()?;
    if files.is_empty() {
        out.push_str("Files: (none)\n");
        return Ok(out);
    }

    out.push_str("Files:\n");
    for file in &files {
        out.push_str(&format!("{}\n", file.display()));
    }

    for file in &files {
        let path_str = file.to_str().unwrap_or("");
        out.push_str(&format!("--- {path_str} ---\n"));
        let content = view.read_file(path_str)?;
        out.push_str(&content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactUpdate, FileChange, create_workspace, integrate};
    use crate::config::ArtifactConfig;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> std::path::PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-show-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    #[test]
    fn show_displays_output_file_contents() {
        let base = temp_path("display");
        let repo_path = base.join("artifact.git");
        let _ = std::fs::remove_dir_all(&base);

        let config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = crate::runtime::load_or_create_artifact(&config).unwrap();

        let workspace_path = base.join("workspace");
        let mut workspace = create_workspace(&artifact, workspace_path);
        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "output.txt".to_string(),
                content: "hello from show\n".to_string(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();
        let integrated = integrate(&artifact, &workspace).unwrap();

        let output = artifact_contents(&integrated).unwrap();

        assert!(output.contains("output.txt"), "output must list the file");
        assert!(
            output.contains("hello from show"),
            "output must include file contents"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
