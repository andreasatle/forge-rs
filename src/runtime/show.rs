use std::error::Error;

use crate::artifacts::{Artifact, ArtifactError, ArtifactView};
use crate::config::ForgeConfig;

/// Display the current artifact contents from the config's artifact repo.
pub fn run_show(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let artifact = super::load_or_create_artifact(&config.artifact, None)?;
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
        match view.read_file(path_str) {
            Ok(content) => {
                out.push_str(&format!("--- {path_str} ---\n"));
                out.push_str(&content);
                if !content.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(ArtifactError::Encoding) => {
                out.push_str(&format!("--- {path_str} --- (binary, skipped)\n"));
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{WorkspaceFileOps, create_workspace, integrate};
    use crate::config::ArtifactConfig;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-show-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    /// Creates a bare repo with main -> Commit A (a.txt), other -> Commit B (b.txt),
    /// and HEAD pointing to other. Returns (bare_repo_path, sha_main, sha_other).
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
        std::fs::write(seed.join("a.txt"), "content on main\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "--quiet", "-m", "Commit A on main"]);
        let sha_main = sha(&["rev-parse", "HEAD"]);

        git(&["checkout", "--quiet", "-b", "other"]);
        std::fs::write(seed.join("b.txt"), "content on other\n").unwrap();
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
    fn show_uses_configured_branch_not_head() {
        let base = temp_path("branch-not-head");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let (repo_path, sha_main, sha_other) = make_two_branch_bare_repo(&base);
        assert_ne!(sha_main, sha_other);

        // HEAD in the bare repo points to "other" (sha_other).
        // We configure branch "main" and expect to see a.txt, not b.txt.
        let artifact = Artifact {
            repo_path: repo_path.canonicalize().unwrap(),
            branch: "main".to_string(),
            commit_sha: sha_main,
        };

        let output = artifact_contents(&artifact).unwrap();

        assert!(
            output.contains("a.txt"),
            "show must list a.txt from main, got: {output}"
        );
        assert!(
            output.contains("content on main"),
            "show must include content from main commit, got: {output}"
        );
        assert!(
            !output.contains("b.txt"),
            "show must not include b.txt from other branch, got: {output}"
        );

        let _ = std::fs::remove_dir_all(&base);
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

        let artifact = crate::runtime::load_or_create_artifact(&config, None).unwrap();

        let workspace_path = base.join("workspace");
        let mut workspace = create_workspace(&artifact, workspace_path);
        workspace
            .write_file("output.txt", "hello from show\n")
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

    #[test]
    fn show_skips_non_utf8_files_instead_of_aborting() {
        let base = temp_path("binary-skip");
        let repo_path = base.join("artifact.git");
        let _ = std::fs::remove_dir_all(&base);

        let config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = crate::runtime::load_or_create_artifact(&config, None).unwrap();

        let workspace_path = base.join("workspace");
        let mut workspace = create_workspace(&artifact, workspace_path);
        workspace.write_file("readme.txt", "hello text\n").unwrap();
        // Non-UTF-8 bytes are written directly since write_file only accepts &str.
        std::fs::write(
            workspace.path().join("bytecode.pyc"),
            [0xFF, 0xFE, 0x00, 0xFF],
        )
        .unwrap();
        let integrated = integrate(&artifact, &workspace).unwrap();

        let output = artifact_contents(&integrated).unwrap();

        assert!(
            output.contains("hello text"),
            "readable file content must still be shown, got: {output}"
        );
        assert!(
            output.contains("--- bytecode.pyc --- (binary, skipped)"),
            "non-UTF-8 file must be reported as skipped, not abort output, got: {output}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
