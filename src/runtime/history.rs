use std::error::Error;
use std::process::Command;

use crate::artifacts::Artifact;
use crate::config::ForgeConfig;

/// Print the artifact commit history (newest first) from the config's artifact repo.
pub fn run_history(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let artifact = super::load_or_create_artifact(&config.artifact)?;
    let log = artifact_log(&artifact)?;
    print!("{log}");
    Ok(())
}

fn artifact_log(artifact: &Artifact) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&artifact.repo_path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ArtifactConfig;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> std::path::PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-history-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    #[test]
    fn history_prints_initial_commit() {
        let path = temp_path("initial");
        let _ = std::fs::remove_dir_all(&path);

        let config = ArtifactConfig {
            repo_path: path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = crate::runtime::load_or_create_artifact(&config).unwrap();
        let log = artifact_log(&artifact).unwrap();

        assert!(
            log.contains("Initial"),
            "history must contain 'Initial', got: {log}"
        );

        let _ = std::fs::remove_dir_all(&path);
    }
}
