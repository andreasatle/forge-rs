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

    let artifact =
        super::load_or_create_artifact(&config.artifact, config.project.language.as_deref())?;

    let short_sha = &artifact.commit_sha[..artifact.commit_sha.len().min(7)];
    println!("Reset complete. Initial commit: {short_sha}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{WorkspaceFileOps, create_workspace, integrate};
    use crate::config::ArtifactConfig;
    use crate::language::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};
    use crate::validation::CommandSpec;
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

    fn make_forge_config(repo_path: &Path, telemetry_path: &Path) -> ForgeConfig {
        use crate::config::{
            ArtifactConfig, ProjectConfig, ProviderConfig, ProviderTierConfig, TelemetryConfig,
            UnmanagedProviderConfig,
        };
        ForgeConfig {
            objective: "test".to_string(),
            artifact: ArtifactConfig {
                repo_path: repo_path.to_str().unwrap().to_string(),
                branch: "main".to_string(),
            },
            provider: ProviderConfig {
                cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                    base_url: "http://localhost:8080".to_string(),
                    model: "llama-test".to_string(),
                    n_predict: 512,
                }),
                strong: None,
                timeout_seconds: 120,
                strong_timeout_seconds: None,
            },
            telemetry: TelemetryConfig {
                directory: telemetry_path.to_str().unwrap().to_string(),
            },
            validation: None,
            project: ProjectConfig::default(),
        }
    }

    fn make_forge_config_with_language(
        repo_path: &Path,
        telemetry_path: &Path,
        language: &str,
    ) -> ForgeConfig {
        use crate::config::{
            ArtifactConfig, ProjectConfig, ProviderConfig, ProviderTierConfig, TelemetryConfig,
            UnmanagedProviderConfig,
        };
        ForgeConfig {
            objective: "test".to_string(),
            artifact: ArtifactConfig {
                repo_path: repo_path.to_str().unwrap().to_string(),
                branch: "main".to_string(),
            },
            provider: ProviderConfig {
                cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                    base_url: "http://localhost:8080".to_string(),
                    model: "llama-test".to_string(),
                    n_predict: 512,
                }),
                strong: None,
                timeout_seconds: 120,
                strong_timeout_seconds: None,
            },
            telemetry: TelemetryConfig {
                directory: telemetry_path.to_str().unwrap().to_string(),
            },
            validation: None,
            project: ProjectConfig {
                kind: crate::config::ProjectKind::Coding,
                language: Some(language.to_string()),
                variant: crate::config::ProjectVariant::Coding,
            },
        }
    }

    fn git_ls_tree_names(repo_path: &PathBuf, commit: &str) -> Vec<String> {
        let out = Command::new("git")
            .args(["ls-tree", "--name-only", "-r", commit])
            .current_dir(repo_path)
            .output()
            .expect("git ls-tree failed");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    fn git_show_file(repo_path: &PathBuf, commit: &str, file: &str) -> String {
        let object = format!("{commit}:{file}");
        let out = Command::new("git")
            .args(["show", &object])
            .current_dir(repo_path)
            .output()
            .expect("git show failed");
        String::from_utf8(out.stdout).unwrap()
    }

    fn register_fake_language(label: &str) -> String {
        let id = format!(
            "fake-reset-{label}-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        crate::language::registry::register_test_language_spec(
            id.clone(),
            LanguageSpec {
                prompt_guidance: "fake language guidance".to_string(),
                init: LanguageInitSpec {
                    gitignore: vec!["ignored-build/".to_string()],
                    commands: vec![
                        CommandSpec {
                            program: "sh".to_string(),
                            args: vec![
                                "-c".to_string(),
                                "printf 'configured artifact\\n' > generated.txt".to_string(),
                            ],
                            when_files_present: vec![],
                            scope: crate::validation::ValidationScope::Workspace,
                        },
                        CommandSpec {
                            program: "sh".to_string(),
                            args: vec![
                                "-c".to_string(),
                                concat!(
                                    "mkdir -p bin ignored-build && ",
                                    "printf 'fake dependency\\n' > manifest.txt && ",
                                    "printf 'ignored\\n' > ignored-build/cache.txt && ",
                                    "printf '#!/bin/sh\\necho fake tool\\n' > bin/fake-tool && ",
                                    "chmod +x bin/fake-tool"
                                )
                                .to_string(),
                            ],
                            when_files_present: vec![],
                            scope: crate::validation::ValidationScope::Workspace,
                        },
                    ],
                },
                validation: LanguageValidationSpec {
                    runs_tests: false,
                    commands: vec![],
                    validation_targets: vec![],
                },
            },
        );
        id
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

        let artifact = crate::runtime::load_or_create_artifact(&artifact_config, None).unwrap();

        let workspace_path = base.join("workspace");
        let mut workspace = create_workspace(&artifact, workspace_path);
        workspace.write_file("extra.txt", "extra\n").unwrap();
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

        let _artifact = crate::runtime::load_or_create_artifact(&artifact_config, None).unwrap();

        let config = make_forge_config(&repo_path, &telemetry);
        run_reset(config).unwrap();

        assert_eq!(
            commit_count(&repo_path, "main"),
            1,
            "only Initial after reset"
        );

        let artifact = crate::runtime::load_or_create_artifact(&artifact_config, None).unwrap();
        let workspace_path = base.join("workspace-post-reset");
        let mut workspace = create_workspace(&artifact, workspace_path);
        workspace
            .write_file("result.txt", "post-reset run\n")
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
            use crate::config::{
                ArtifactConfig, ProjectConfig, ProviderConfig, ProviderTierConfig, TelemetryConfig,
                UnmanagedProviderConfig,
            };
            ForgeConfig {
                objective: "test".to_string(),
                artifact: ArtifactConfig {
                    repo_path: repo_path.to_str().unwrap().to_string(),
                    branch: "artifact".to_string(),
                },
                provider: ProviderConfig {
                    cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                        base_url: "http://localhost:8080".to_string(),
                        model: "llama-test".to_string(),
                        n_predict: 512,
                    }),
                    strong: None,
                    timeout_seconds: 120,
                    strong_timeout_seconds: None,
                },
                telemetry: TelemetryConfig {
                    directory: telemetry.to_str().unwrap().to_string(),
                },
                validation: None,
                project: ProjectConfig::default(),
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

    // ── language-init reset tests ─────────────────────────────────────────────

    #[test]
    fn reset_removes_existing_artifact_git_repo() {
        let base = temp_path("removes-existing");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        // Create a repo with content so we can verify it is fully replaced.
        let artifact_config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };
        crate::runtime::load_or_create_artifact(&artifact_config, None).unwrap();
        assert!(repo_path.exists(), "repo must exist before reset");

        let config = make_forge_config(&repo_path, &telemetry);
        run_reset(config).unwrap();

        assert!(
            repo_path.exists(),
            "reset must recreate the repo at the same path"
        );
        assert_eq!(
            commit_count(&repo_path, "main"),
            1,
            "reset must produce a fresh repo with exactly one commit"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reset_recreates_bare_artifact_repo() {
        let base = temp_path("recreates-bare");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        let config = make_forge_config(&repo_path, &telemetry);
        run_reset(config).unwrap();

        // Verify it is a valid bare git repository.
        let out = Command::new("git")
            .args(["rev-parse", "--is-bare-repository"])
            .current_dir(&repo_path)
            .output()
            .expect("git rev-parse failed");
        assert!(out.status.success(), "reset repo must be a valid git repo");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap().trim(),
            "true",
            "reset repo must be a bare repository"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn language_reset_runs_init_before_initial_commit() {
        let language = register_fake_language("init-order");
        let base = temp_path("language-init-order");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        run_reset(make_forge_config_with_language(
            &repo_path, &telemetry, &language,
        ))
        .unwrap();

        // Language init must produce exactly one commit — language files ARE the
        // initial commit, not a follow-up "Integrate artifact update" commit.
        assert_eq!(
            commit_count(&repo_path, "main"),
            1,
            "language reset must produce exactly one initial commit containing language files"
        );

        let files = git_ls_tree_names(&repo_path, "HEAD");
        assert!(
            files.iter().any(|f| f == "generated.txt"),
            "initial commit must contain configured generated file; files found: {files:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn language_reset_commits_configured_init_outputs() {
        let language = register_fake_language("init-outputs");
        let base = temp_path("language-init-outputs");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        run_reset(make_forge_config_with_language(
            &repo_path, &telemetry, &language,
        ))
        .unwrap();

        let generated = git_show_file(&repo_path, "HEAD", "generated.txt");
        assert_eq!(generated, "configured artifact\n");

        let manifest = git_show_file(&repo_path, "HEAD", "manifest.txt");
        assert_eq!(manifest, "fake dependency\n");

        let files = git_ls_tree_names(&repo_path, "HEAD");
        assert!(
            !files.iter().any(|f| f == "ignored-build/cache.txt"),
            "gitignore entries from the language spec must exclude generated cache files; files found: {files:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn language_reset_workspace_contains_configured_tool_outputs() {
        let language = register_fake_language("tool-output");
        let base = temp_path("language-tool-output");
        let repo_path = base.join("artifact.git");
        let telemetry = base.join("telemetry");
        let _ = std::fs::remove_dir_all(&base);

        run_reset(make_forge_config_with_language(
            &repo_path, &telemetry, &language,
        ))
        .unwrap();

        // Load the reset artifact and create a workspace from it.
        let artifact_config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };
        let artifact = crate::runtime::load_or_create_artifact(&artifact_config, None).unwrap();

        let workspace_path = base.join("workspace");
        let ws = crate::artifacts::create_workspace(&artifact, workspace_path);

        let output = Command::new("./bin/fake-tool")
            .current_dir(ws.path())
            .output()
            .expect("failed to spawn configured fake tool");

        assert!(
            output.status.success(),
            "configured fake tool must run from artifact workspace"
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "fake tool\n");

        let _ = std::fs::remove_dir_all(&base);
    }
}
