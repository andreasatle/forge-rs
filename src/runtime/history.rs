use std::error::Error;
use std::process::Command;

use crate::artifacts::Artifact;
use crate::config::ForgeConfig;

/// Print the artifact commit history (newest first) from the config's artifact repo.
pub fn run_history(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let artifact = super::load_or_create_artifact(&config.artifact, None)?;
    let log = artifact_log(&artifact)?;
    print!("{log}");
    Ok(())
}

fn artifact_log(artifact: &Artifact) -> Result<String, Box<dyn Error>> {
    let branch_ref = format!("refs/heads/{}", artifact.branch);
    let output = Command::new("git")
        .args(["log", "--oneline", &branch_ref])
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
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-history-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    /// Same two-branch setup as run.rs tests: main -> A, other -> B, HEAD -> other.
    fn make_two_branch_bare_repo(base: &Path) -> (PathBuf, String, String) {
        let seed = base.join("seed");
        std::fs::create_dir_all(&seed).unwrap();

        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&seed)
                    .status()
                    .unwrap()
                    .success(),
                "git {} failed",
                args.join(" ")
            );
        };
        let sha = |args: &[&str]| -> String {
            String::from_utf8(
                Command::new("git")
                    .args(args)
                    .current_dir(&seed)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned()
        };

        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Forge Test"]);
        git(&["config", "user.email", "forge-test@example.invalid"]);
        std::fs::write(seed.join("a.txt"), "on main\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "--quiet", "-m", "Commit A on main"]);
        let sha_main = sha(&["rev-parse", "HEAD"]);

        git(&["checkout", "--quiet", "-b", "other"]);
        std::fs::write(seed.join("b.txt"), "on other\n").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "--quiet", "-m", "Commit B on other"]);
        let sha_other = sha(&["rev-parse", "HEAD"]);

        let bare = base.join("artifact.git");
        assert!(
            Command::new("git")
                .args(["clone", "--quiet", "--bare"])
                .arg(&seed)
                .arg(&bare)
                .status()
                .unwrap()
                .success(),
            "git clone --bare failed"
        );

        (bare, sha_main, sha_other)
    }

    #[test]
    fn history_prints_initial_commit() {
        let path = temp_path("initial");
        let _ = std::fs::remove_dir_all(&path);

        let config = ArtifactConfig {
            repo_path: path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = crate::runtime::load_or_create_artifact(&config, None).unwrap();
        let log = artifact_log(&artifact).unwrap();

        assert!(
            log.contains("Initial"),
            "history must contain 'Initial', got: {log}"
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn history_uses_configured_branch_not_head() {
        let base = temp_path("branch-not-head");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let (repo_path, sha_main, sha_other) = make_two_branch_bare_repo(&base);
        assert_ne!(sha_main, sha_other);

        // HEAD points to "other" (sha_other); we want history for "main".
        let artifact = Artifact {
            repo_path: repo_path.canonicalize().unwrap(),
            branch: "main".to_string(),
            commit_sha: sha_main.clone(),
        };

        let log = artifact_log(&artifact).unwrap();

        assert!(
            log.contains("Commit A on main"),
            "history must list the main commit, got: {log}"
        );
        assert!(
            !log.contains("Commit B on other"),
            "history must not include the other-branch commit, got: {log}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
