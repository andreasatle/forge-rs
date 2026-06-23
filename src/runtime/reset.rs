use std::error::Error;
use std::path::{Path, PathBuf};

use crate::config::ForgeConfig;

fn validate_reset_path(repo_path: &Path) -> Result<(), Box<dyn Error>> {
    // Canonicalize for comparisons when the path already exists.
    let canonical = if repo_path.exists() {
        repo_path.canonicalize()?
    } else {
        repo_path.to_path_buf()
    };

    // Must not be the filesystem root.
    if canonical == Path::new("/") {
        return Err("reset refused: repo_path must not be the filesystem root".into());
    }

    // Must not be the user's home directory.
    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(&home);
        let home_canonical = if home_path.exists() {
            home_path.canonicalize().unwrap_or(home_path)
        } else {
            home_path
        };
        if canonical == home_canonical {
            return Err("reset refused: repo_path must not be the home directory".into());
        }
    }

    // Must not be the current working directory.
    if let Ok(cwd) = std::env::current_dir()
        && canonical == cwd
    {
        return Err("reset refused: repo_path must not be the current working directory".into());
    }

    // Must end with .git — bare artifact repositories always carry this suffix.
    let path_str = repo_path.to_str().ok_or("repo_path is not valid UTF-8")?;
    if !path_str.ends_with(".git") {
        return Err(format!("reset refused: repo_path must end with .git, got: {path_str}").into());
    }

    Ok(())
}

/// Delete the artifact repository and recreate it with only the Initial commit.
pub fn run_reset(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let repo_path = PathBuf::from(&config.artifact.repo_path);

    validate_reset_path(&repo_path)?;

    if repo_path.exists() {
        std::fs::remove_dir_all(&repo_path)?;
    }

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

    fn commit_count(repo_path: &PathBuf, branch: &str) -> usize {
        let branch_ref = format!("refs/heads/{branch}");
        let out = Command::new("git")
            .args(["rev-list", "--count", &branch_ref])
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
    fn reset_rejects_root_path() {
        let root = PathBuf::from("/");
        let result = validate_reset_path(&root);
        assert!(result.is_err(), "reset must be refused for root path");
        assert!(
            result.unwrap_err().to_string().contains("reset refused"),
            "error must contain 'reset refused'"
        );
    }

    #[test]
    fn reset_rejects_home_directory() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let home_path = PathBuf::from(&home);
        let result = validate_reset_path(&home_path);
        assert!(
            result.is_err(),
            "reset must be refused for home directory path"
        );
    }

    #[test]
    fn reset_rejects_non_git_path() {
        let path = PathBuf::from("/tmp/not-a-git-repo");
        let result = validate_reset_path(&path);
        assert!(result.is_err(), "reset must be refused for non-.git path");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains(".git"),
            "error must mention .git requirement, got: {msg}"
        );
    }

    #[test]
    fn reset_accepts_configured_artifact_git_path() {
        let base = temp_path("accept-valid");
        let repo_path = base.join("artifact.git");
        let result = validate_reset_path(&repo_path);
        assert!(
            result.is_ok(),
            "reset must be accepted for valid .git path in temp dir, got: {result:?}"
        );
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
        let artifact = integrate(&artifact, &workspace).unwrap();

        assert_eq!(
            commit_count(&artifact.repo_path, "main"),
            2,
            "should have two commits before reset"
        );

        let config = make_forge_config(&artifact.repo_path, &telemetry);
        run_reset(config).unwrap();

        assert_eq!(
            commit_count(&repo_path, "main"),
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

        assert_eq!(
            commit_count(&repo_path, "main"),
            1,
            "only Initial after reset"
        );

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
        let final_artifact = integrate(&artifact, &workspace).unwrap();

        assert_eq!(
            commit_count(&final_artifact.repo_path, "main"),
            2,
            "run after reset must produce a new commit"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reset_creates_configured_branch() {
        let base = temp_path("configured-branch");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        // Use a non-default branch name to prove reset uses the config, not "main".
        let config = {
            use crate::config::{ArtifactConfig, ProviderConfig, TelemetryConfig};
            ForgeConfig {
                objective: "test".to_string(),
                artifact: ArtifactConfig {
                    repo_path: repo_path.to_str().unwrap().to_string(),
                    branch: "artifact".to_string(),
                },
                provider: ProviderConfig {
                    base_url: "http://localhost:8080".to_string(),
                    n_predict: 512,
                },
                telemetry: TelemetryConfig {
                    directory: telemetry.to_str().unwrap().to_string(),
                },
            }
        };

        run_reset(config).unwrap();

        // The configured branch must exist and have exactly one commit.
        assert_eq!(
            commit_count(&repo_path, "artifact"),
            1,
            "reset must create the configured branch with Initial commit"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
