use super::*;
use crate::artifacts::{WorkspaceFactory, WorkspaceFileOps, integrate};
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
    let out = crate::git::command()
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
        ArtifactConfig, ProviderConfig, ProviderTierConfig, TelemetryConfig,
        UnmanagedProviderConfig,
    };
    ForgeConfig {
        objective: Some("test".to_string()),
        teams: Vec::new(),
        terminal_teams: Vec::new(),
        artifact: ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        },
        provider: ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "llama-test".to_string(),
                n_predict: 512,
                parallel: 1,
            }),
            strong: None,
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        },
        telemetry: TelemetryConfig {
            directory: telemetry_path.to_str().unwrap().to_string(),
        },
        validation: None,
        adapter: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("coding.yaml")
            .to_str()
            .unwrap()
            .to_string(),
        language: "py".to_string(),
        dispatch_cap: 1,
    }
}

/// A minimal adapter config with one "implementer" worker role and a
/// `plugins:` list naming `plugin_id`, written to `dir/adapter.yaml`.
fn write_adapter_with_plugin(dir: &Path, plugin_id: &str) -> PathBuf {
    let yaml = format!(
        r#"
planner:
  producer:
    identity: "identity"
    context: "context"
    instructions: "instructions"
    constraints: "constraints"
  critic:
    identity: "identity"
    context: "context"
    instructions: "instructions"
    constraints: "constraints"
  referee:
    identity: "identity"
    context: "context"
    instructions: "instructions"
    constraints: "constraints"
workers:
  - key: implementer
    description: "Implements code."
    producer:
      identity: "identity"
      context: "context"
      instructions: "instructions"
      constraints: "constraints"
    critic:
      identity: "identity"
      context: "context"
      instructions: "instructions"
      constraints: "constraints"
    referee:
      identity: "identity"
      context: "context"
      instructions: "instructions"
      constraints: "constraints"
plugins:
  - {plugin_id}
"#
    );
    let path = dir.join("adapter.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Builds a `ForgeConfig` whose adapter (written into `dir`) declares
/// `language` as its sole plugin, so `run_reset` bootstraps the artifact
/// repo using that language's init commands.
fn make_forge_config_with_language(
    repo_path: &Path,
    telemetry_path: &Path,
    dir: &Path,
    language: &str,
) -> ForgeConfig {
    use crate::config::{
        ArtifactConfig, ProviderConfig, ProviderTierConfig, TelemetryConfig,
        UnmanagedProviderConfig,
    };
    std::fs::create_dir_all(dir).unwrap();
    let adapter_path = write_adapter_with_plugin(dir, language);
    ForgeConfig {
        objective: Some("test".to_string()),
        teams: Vec::new(),
        terminal_teams: Vec::new(),
        artifact: ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        },
        provider: ProviderConfig {
            cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                model: "llama-test".to_string(),
                n_predict: 512,
                parallel: 1,
            }),
            strong: None,
            timeout_seconds: 120,
            strong_timeout_seconds: None,
        },
        telemetry: TelemetryConfig {
            directory: telemetry_path.to_str().unwrap().to_string(),
        },
        validation: None,
        adapter: adapter_path.to_str().unwrap().to_string(),
        language: "fake".to_string(),
        dispatch_cap: 1,
    }
}

fn git_ls_tree_names(repo_path: &PathBuf, commit: &str) -> Vec<String> {
    let out = crate::git::command()
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
    let out = crate::git::command()
        .args(["show", &object])
        .current_dir(repo_path)
        .output()
        .expect("git show failed");
    String::from_utf8(out.stdout).unwrap()
}

/// Registers a fake language spec under the path `dir/<id>` would resolve to
/// (`dir` being the plugin-declaring adapter's own directory) and returns the
/// bare `id` to write into that adapter's `plugins:` list.
fn register_fake_language(dir: &Path, label: &str) -> String {
    let id = format!(
        "fake-reset-{label}-{}",
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let resolved = dir.join(&id).to_string_lossy().into_owned();
    crate::language::registry::register_test_language_spec(
        resolved,
        LanguageSpec {
            extensions: vec!["fake".to_string()],
            identity: "fake language guidance".to_string(),
            context: String::new(),
            instructions: String::new(),
            constraints: String::new(),
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
            functions: std::collections::BTreeMap::new(),
            api_summary: None,
        },
    );
    id
}

#[test]
fn reset_rejects_root_path() {
    let root = PathBuf::from("/");
    let result = validate_reset_path(&root);
    assert!(result.is_err(), "reset must be refused for root path");
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
    let mut workspace = WorkspaceFactory::new(&artifact).create_workspace(workspace_path);
    workspace.write_file("extra.txt", "extra\n").unwrap();
    let artifact = integrate(&artifact, &workspace).unwrap();

    assert_eq!(
        commit_count(&artifact.repo_path, "main"),
        2,
        "should have two commits before reset"
    );

    let config = make_forge_config(&artifact.repo_path, &telemetry);
    run_reset(config).unwrap();

    assert!(
        repo_path.exists(),
        "reset must recreate the repo at the same path"
    );
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
    let mut workspace = WorkspaceFactory::new(&artifact).create_workspace(workspace_path);
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
            ArtifactConfig, ProviderConfig, ProviderTierConfig, TelemetryConfig,
            UnmanagedProviderConfig,
        };
        ForgeConfig {
            objective: Some("test".to_string()),
            teams: Vec::new(),
            terminal_teams: Vec::new(),
            artifact: ArtifactConfig {
                repo_path: repo_path.to_str().unwrap().to_string(),
                branch: "artifact".to_string(),
            },
            provider: ProviderConfig {
                cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
                    base_url: "http://localhost:8080".to_string(),
                    model: "llama-test".to_string(),
                    n_predict: 512,
                    parallel: 1,
                }),
                strong: None,
                timeout_seconds: 120,
                strong_timeout_seconds: None,
            },
            telemetry: TelemetryConfig {
                directory: telemetry.to_str().unwrap().to_string(),
            },
            validation: None,
            adapter: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join("coding.yaml")
                .to_str()
                .unwrap()
                .to_string(),
            language: "py".to_string(),
            dispatch_cap: 1,
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
fn reset_recreates_bare_artifact_repo() {
    let base = temp_path("recreates-bare");
    let repo_path = base.join("artifact.git");
    let telemetry = base.join("telemetry");
    let _ = std::fs::remove_dir_all(&base);

    let config = make_forge_config(&repo_path, &telemetry);
    run_reset(config).unwrap();

    // Verify it is a valid bare git repository.
    let out = crate::git::command()
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
    let base = temp_path("language-init-order");
    let repo_path = base.join("artifact.git");
    let telemetry = base.join("telemetry");
    let _ = std::fs::remove_dir_all(&base);
    let language = register_fake_language(&base, "init-order");

    run_reset(make_forge_config_with_language(
        &repo_path, &telemetry, &base, &language,
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
    let base = temp_path("language-init-outputs");
    let repo_path = base.join("artifact.git");
    let telemetry = base.join("telemetry");
    let _ = std::fs::remove_dir_all(&base);
    let language = register_fake_language(&base, "init-outputs");

    run_reset(make_forge_config_with_language(
        &repo_path, &telemetry, &base, &language,
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
    let base = temp_path("language-tool-output");
    let repo_path = base.join("artifact.git");
    let telemetry = base.join("telemetry");
    let _ = std::fs::remove_dir_all(&base);
    let language = register_fake_language(&base, "tool-output");

    run_reset(make_forge_config_with_language(
        &repo_path, &telemetry, &base, &language,
    ))
    .unwrap();

    // Load the reset artifact and create a workspace from it.
    let artifact_config = ArtifactConfig {
        repo_path: repo_path.to_str().unwrap().to_string(),
        branch: "main".to_string(),
    };
    let artifact = crate::runtime::load_or_create_artifact(&artifact_config, None).unwrap();

    let workspace_path = base.join("workspace");
    let ws = crate::artifacts::WorkspaceFactory::new(&artifact).create_workspace(workspace_path);

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
