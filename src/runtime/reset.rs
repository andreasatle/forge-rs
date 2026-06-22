use std::error::Error;
use std::path::PathBuf;

use crate::config::ForgeConfig;

/// Delete the artifact repository and recreate it with only the Initial commit.
pub fn run_reset(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let repo_path = PathBuf::from(&config.artifact.repo_path);

    if repo_path.exists() {
        std::fs::remove_dir_all(&repo_path)?;
    }

    let telemetry_dir = PathBuf::from(&config.telemetry.directory);
    let _ = std::fs::remove_dir_all(&telemetry_dir);

    let artifact = super::load_or_create_artifact(&config.artifact)?;

    let short_sha = &artifact.commit_sha[..artifact.commit_sha.len().min(7)];
    println!("Reset complete. Initial commit: {short_sha}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactUpdate, FileChange, create_workspace, integrate};
    use crate::config::ArtifactConfig;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-reset-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    fn commit_count(repo_path: &PathBuf) -> usize {
        let out = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(repo_path)
            .output()
            .expect("git rev-list failed");
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap_or(0)
    }

    fn make_forge_config(repo_path: &PathBuf, telemetry_path: &PathBuf) -> ForgeConfig {
        use crate::config::{ArtifactConfig, ProviderConfig, TelemetryConfig};
        ForgeConfig {
            objective: "test".to_string(),
            artifact: ArtifactConfig {
                repo_path: repo_path.to_str().unwrap().to_string(),
                branch: "main".to_string(),
            },
            provider: ProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                n_predict: 512,
            },
            telemetry: TelemetryConfig {
                directory: telemetry_path.to_str().unwrap().to_string(),
            },
        }
    }

    #[test]
    fn reset_recreates_initial_commit() {
        let base = temp_path("recreate");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        let artifact_config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = crate::runtime::load_or_create_artifact(&artifact_config).unwrap();

        let workspace_path = base.join("workspace");
        let mut workspace = create_workspace(&artifact, workspace_path);
        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "extra.txt".to_string(),
                content: "extra\n".to_string(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();
        let artifact = integrate(&artifact, &workspace);

        assert_eq!(
            commit_count(&artifact.repo_path),
            2,
            "should have two commits before reset"
        );

        let config = make_forge_config(&artifact.repo_path, &telemetry);
        run_reset(config).unwrap();

        assert_eq!(
            commit_count(&repo_path),
            1,
            "after reset only Initial commit must remain"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn run_after_reset_creates_new_commit() {
        let base = temp_path("run-after");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        let artifact_config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = crate::runtime::load_or_create_artifact(&artifact_config).unwrap();

        let config = make_forge_config(&repo_path, &telemetry);
        run_reset(config).unwrap();

        assert_eq!(commit_count(&repo_path), 1, "only Initial after reset");

        let artifact = crate::runtime::load_or_create_artifact(&artifact_config).unwrap();
        let workspace_path = base.join("workspace-post-reset");
        let mut workspace = create_workspace(&artifact, workspace_path);
        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "result.txt".to_string(),
                content: "post-reset run\n".to_string(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();
        let final_artifact = integrate(&artifact, &workspace);

        assert_eq!(
            commit_count(&final_artifact.repo_path),
            2,
            "run after reset must produce a new commit"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
