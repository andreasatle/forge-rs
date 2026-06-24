//! Forge runtime — wires config into machines and drives a single run.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static SEED_COUNTER: AtomicU64 = AtomicU64::new(0);

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::{ArtifactConfig, ForgeConfig, ValidationConfig};
use crate::engine::run_machine_with_telemetry;
use crate::machines::scheduler::state::SchedulerState;
use crate::machines::scheduler::{RunRequest, SchedulerHandler, SchedulerMachine, SchedulerOutput};
use crate::node_runner::DeliberatingNodeRunner;
use crate::project::{DefaultProjectAdapter, ProjectAdapter};
use crate::providers::{LlamaCppProvider, RetryingProvider};
use crate::runtime::checkpoint::node_counts;
use crate::runtime::resume::find_resumable_run;
use crate::runtime::{create_run, finalize_manifest};
use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::validation::{AlwaysPassValidator, CommandValidator, Validator};

/// Entry point for a single forge run driven by a [`ForgeConfig`].
pub struct ForgeRuntime;

impl ForgeRuntime {
    /// Run forge to completion using the given config.
    ///
    /// Responsibilities:
    /// 1. Load or create the bare artifact repository.
    /// 2. Create the telemetry sink.
    /// 3. Build the provider stack.
    /// 4. Drive the scheduler to completion.
    /// 5. Print a summary to stdout.
    pub fn run(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
        let artifact = load_or_create_artifact(&config.artifact)?;

        let runs_root = PathBuf::from(&config.telemetry.directory);
        let run_info = create_run(
            &runs_root,
            &config.objective,
            &config.artifact.repo_path,
            &config.provider.base_url,
        )?;
        let sink: Rc<dyn TelemetrySink> =
            Rc::new(FileTelemetry::new(run_info.telemetry_dir.clone()));

        let cheap_llama =
            LlamaCppProvider::new(&config.provider.base_url, config.provider.timeout_seconds);
        let cheap = RetryingProvider::new(cheap_llama, 3);

        let strong_base_url = config
            .provider
            .strong_base_url
            .as_deref()
            .unwrap_or(&config.provider.base_url);
        let strong_timeout = config
            .provider
            .strong_timeout_seconds
            .unwrap_or(config.provider.timeout_seconds);
        let strong_llama = LlamaCppProvider::new(strong_base_url, strong_timeout);
        let strong = RetryingProvider::new(strong_llama, 3);

        let cheap_tokens = config.provider.n_predict as u32;
        let strong_tokens = config
            .provider
            .strong_n_predict
            .unwrap_or(config.provider.n_predict) as u32;

        let role_policy = DefaultProjectAdapter.role_policy();
        let runner = DeliberatingNodeRunner::new(cheap, strong)
            .with_cheap_max_tokens(cheap_tokens)
            .with_strong_max_tokens(strong_tokens)
            .with_role_policy(role_policy);
        let validator = make_validator(config.validation.as_ref());
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator)
            .with_checkpoint_dir(run_info.run_dir.clone());

        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: config.objective.clone(),
        });

        let (output, handler) = run_machine_with_telemetry(handler, initial_state, sink.as_ref());

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        let (status, final_commit, failure_reason) = match &output {
            SchedulerOutput::Complete { .. } => (
                "succeeded",
                final_artifact.as_ref().map(|a| a.commit_sha.as_str()),
                None,
            ),
            SchedulerOutput::Failed { reason, .. } => ("failed", None, Some(reason.as_str())),
        };
        if let Err(e) = finalize_manifest(
            &run_info,
            status,
            final_commit,
            validation_passed,
            failure_reason,
        ) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        runtime_result_from_scheduler_output(output)
    }

    /// Resume a previously interrupted forge run.
    ///
    /// Scans `config.telemetry.directory` for a run whose `manifest.json` has
    /// `status == "running"` and loads its `graph.json` checkpoint. Exactly one
    /// such run must exist; zero or multiple produce a clear error.
    ///
    /// The restored state is normalized before re-entry: any node that was
    /// mid-execution at crash time is reset to `Pending` so the scheduler
    /// re-dispatches it. Completed work (durable in git) is preserved.
    pub fn resume(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
        let runs_root = PathBuf::from(&config.telemetry.directory);
        let (run_dir, initial_state) = find_resumable_run(&runs_root)?;

        let artifact = load_or_create_artifact(&config.artifact)?;
        let sink: Rc<dyn TelemetrySink> = Rc::new(FileTelemetry::new(run_dir.join("telemetry")));

        let graph = match &initial_state {
            SchedulerState::Running { graph } => graph,
            _ => unreachable!("normalize_for_resume always returns Running"),
        };
        let (node_count, completed_count) = node_counts(graph);
        sink.record(TelemetryRecord::new(
            "Checkpoint",
            TelemetryEvent::CheckpointLoaded {
                node_count,
                completed_count,
            },
        ));

        let cheap_llama =
            LlamaCppProvider::new(&config.provider.base_url, config.provider.timeout_seconds);
        let cheap = RetryingProvider::new(cheap_llama, 3);

        let strong_base_url = config
            .provider
            .strong_base_url
            .as_deref()
            .unwrap_or(&config.provider.base_url);
        let strong_timeout = config
            .provider
            .strong_timeout_seconds
            .unwrap_or(config.provider.timeout_seconds);
        let strong_llama = LlamaCppProvider::new(strong_base_url, strong_timeout);
        let strong = RetryingProvider::new(strong_llama, 3);

        let cheap_tokens = config.provider.n_predict as u32;
        let strong_tokens = config
            .provider
            .strong_n_predict
            .unwrap_or(config.provider.n_predict) as u32;

        let role_policy = DefaultProjectAdapter.role_policy();
        let runner = DeliberatingNodeRunner::new(cheap, strong)
            .with_cheap_max_tokens(cheap_tokens)
            .with_strong_max_tokens(strong_tokens)
            .with_role_policy(role_policy);
        let validator = make_validator(config.validation.as_ref());
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator)
            .with_checkpoint_dir(run_dir.clone());

        let run_info = crate::runtime::RunInfo {
            run_id: run_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            run_dir: run_dir.clone(),
            telemetry_dir: run_dir.join("telemetry"),
            started_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        };

        let (output, handler) = run_machine_with_telemetry(handler, initial_state, sink.as_ref());

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        let (status, final_commit, failure_reason) = match &output {
            SchedulerOutput::Complete { .. } => (
                "succeeded",
                final_artifact.as_ref().map(|a| a.commit_sha.as_str()),
                None,
            ),
            SchedulerOutput::Failed { reason, .. } => ("failed", None, Some(reason.as_str())),
        };
        if let Err(e) = finalize_manifest(
            &run_info,
            status,
            final_commit,
            validation_passed,
            failure_reason,
        ) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        runtime_result_from_scheduler_output(output)
    }
}

fn runtime_result_from_scheduler_output(output: SchedulerOutput) -> Result<(), Box<dyn Error>> {
    match output {
        SchedulerOutput::Failed { reason, .. } => Err(format!("run failed: {reason}").into()),
        SchedulerOutput::Complete { .. } => Ok(()),
    }
}

fn make_validator(config: Option<&ValidationConfig>) -> Rc<dyn Validator> {
    match config {
        Some(v) if !v.commands.is_empty() => {
            let timeout = Duration::from_secs(v.timeout_seconds.unwrap_or(120));
            Rc::new(CommandValidator::new(v.commands.clone(), timeout))
        }
        _ => Rc::new(AlwaysPassValidator),
    }
}

/// Load the artifact at `config.repo_path`, creating a bare repo if it does not exist.
pub fn load_or_create_artifact(config: &ArtifactConfig) -> Result<Artifact, Box<dyn Error>> {
    let repo_path = PathBuf::from(&config.repo_path);

    if !repo_path.exists() {
        create_bare_repo(&repo_path, &config.branch)?;
    }

    let repo_path = repo_path.canonicalize()?;
    let commit_sha = git_rev_parse_branch(&repo_path, &config.branch)?;

    Ok(Artifact {
        repo_path,
        branch: config.branch.clone(),
        commit_sha,
    })
}

fn create_bare_repo(path: &Path, branch: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let seq = SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = std::env::temp_dir().join(format!("forge-seed-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&seed)?;

    run_git(
        &seed,
        &["init", "--quiet", &format!("--initial-branch={branch}")],
    )?;
    run_git(&seed, &["config", "user.name", "Forge"])?;
    run_git(&seed, &["config", "user.email", "forge@localhost"])?;
    run_git(
        &seed,
        &["commit", "--allow-empty", "--quiet", "-m", "Initial"],
    )?;

    let status = Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(path)
        .status()?;

    let _ = std::fs::remove_dir_all(&seed);

    if !status.success() {
        return Err("git clone --bare failed".into());
    }

    Ok(())
}

fn run_git(path: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let status = Command::new("git").args(args).current_dir(path).status()?;
    if !status.success() {
        return Err(format!("git {} failed", args.join(" ")).into());
    }
    Ok(())
}

fn git_rev_parse_branch(repo_path: &Path, branch: &str) -> Result<String, Box<dyn Error>> {
    let refspec = format!("refs/heads/{branch}");
    let output = Command::new("git")
        .args(["rev-parse", &refspec])
        .current_dir(repo_path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "branch '{branch}' not found in artifact repository at {}",
            repo_path.display()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn print_summary(
    output: &SchedulerOutput,
    config: &ForgeConfig,
    artifact: Option<&Artifact>,
    run_info: &crate::runtime::RunInfo,
) {
    let result_str = match output {
        SchedulerOutput::Complete { .. } => "COMPLETE",
        SchedulerOutput::Failed { .. } => "FAILED",
    };

    println!("Result      : {result_str}");
    println!("Run ID      : {}", run_info.run_id);
    println!("Artifact repo: {}", config.artifact.repo_path);

    if let Some(a) = artifact {
        let short_sha = &a.commit_sha[..a.commit_sha.len().min(7)];
        println!("Commit      : {short_sha}");
        println!("Telemetry   : {}", run_info.telemetry_dir.display());

        let view = ArtifactView {
            repo_path: a.repo_path.clone(),
            commit_sha: a.commit_sha.clone(),
        };
        if let Ok(files) = view.list_files()
            && !files.is_empty()
        {
            println!("\nGenerated files:");
            for f in &files {
                println!("  {}", f.display());
            }
        }
    } else {
        println!("Commit      : unknown");
        println!("Telemetry   : {}", run_info.telemetry_dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ArtifactConfig, ForgeConfig, ProviderConfig, TelemetryConfig};
    use crate::machines::scheduler::machine::{RecoverySummary, SchedulerOutput};
    use crate::machines::scheduler::state::RunGraph;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_path(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-runtime-test-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    fn artifact_config(path: &PathBuf) -> ArtifactConfig {
        ArtifactConfig {
            repo_path: path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        }
    }

    fn empty_graph() -> RunGraph {
        RunGraph {
            nodes: vec![],
            next_id: 0,
        }
    }

    #[test]
    fn runtime_threads_provider_timeout() {
        // Verify that timeout_seconds from config reaches the provider constructor.
        // No live HTTP: we only check that the value is read from config and the
        // provider is constructed without error.
        let config = ForgeConfig {
            objective: "test".to_string(),
            artifact: ArtifactConfig {
                repo_path: "/tmp/test.git".to_string(),
                branch: "main".to_string(),
            },
            provider: ProviderConfig {
                base_url: "http://localhost:8080".to_string(),
                n_predict: 512,
                timeout_seconds: 42,
                strong_base_url: None,
                strong_n_predict: None,
                strong_timeout_seconds: None,
            },
            telemetry: TelemetryConfig {
                directory: "/tmp/telemetry".to_string(),
            },
            validation: None,
        };
        assert_eq!(config.provider.timeout_seconds, 42);
        let _provider =
            LlamaCppProvider::new(&config.provider.base_url, config.provider.timeout_seconds);
        // Construction succeeds: the timeout is wired through.
    }

    #[test]
    fn strong_tier_falls_back_when_no_strong_provider_configured() {
        let config = ProviderConfig {
            base_url: "http://localhost:8080".to_string(),
            n_predict: 512,
            timeout_seconds: 120,
            strong_base_url: None,
            strong_n_predict: None,
            strong_timeout_seconds: None,
        };
        // When strong fields are absent, both tiers must resolve to the cheap values.
        let strong_url = config
            .strong_base_url
            .as_deref()
            .unwrap_or(&config.base_url);
        let strong_tokens = config.strong_n_predict.unwrap_or(config.n_predict);
        let strong_timeout = config
            .strong_timeout_seconds
            .unwrap_or(config.timeout_seconds);
        assert_eq!(strong_url, "http://localhost:8080");
        assert_eq!(strong_tokens, 512);
        assert_eq!(strong_timeout, 120);
    }

    #[test]
    fn failed_runtime_run_returns_error_or_nonzero_status() {
        let output = SchedulerOutput::Failed {
            graph: empty_graph(),
            reason: "something went wrong".to_string(),
        };
        let result = runtime_result_from_scheduler_output(output);
        assert!(result.is_err(), "Failed output must produce an error");
        assert!(
            result.unwrap_err().to_string().contains("run failed"),
            "error message must mention run failed"
        );
    }

    #[test]
    fn runtime_error_includes_provider_failure_reason() {
        let output = SchedulerOutput::Failed {
            graph: empty_graph(),
            reason: "deliberation failed: provider error (Retryable): connection refused"
                .to_string(),
        };
        let result = runtime_result_from_scheduler_output(output);
        let err = result.expect_err("failed output must become an error");
        let message = err.to_string();
        assert!(message.contains("run failed"));
        assert!(message.contains("provider error (Retryable): connection refused"));
    }

    #[test]
    fn successful_runtime_run_still_returns_ok() {
        let output = SchedulerOutput::Complete {
            graph: empty_graph(),
            recovery_summary: RecoverySummary {
                recovered: false,
                retry_count: 0,
                elevate_count: 0,
                split_count: 0,
            },
        };
        let result = runtime_result_from_scheduler_output(output);
        assert!(result.is_ok(), "Complete output must return Ok");
    }

    #[test]
    fn load_or_create_artifact_creates_missing_repo() {
        let path = temp_path("create-missing");
        let _ = std::fs::remove_dir_all(&path);

        let config = artifact_config(&path);
        let result = load_or_create_artifact(&config);

        assert!(result.is_ok(), "expected artifact creation to succeed");
        assert!(path.exists(), "bare repo directory must be created");

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn load_or_create_artifact_sets_branch() {
        let path = temp_path("branch");
        let _ = std::fs::remove_dir_all(&path);

        let config = artifact_config(&path);
        let artifact = load_or_create_artifact(&config).unwrap();

        assert_eq!(artifact.branch, "main");

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn load_or_create_artifact_loads_existing_repo() {
        let path = temp_path("load-existing");
        let _ = std::fs::remove_dir_all(&path);

        let config = artifact_config(&path);
        let first = load_or_create_artifact(&config).unwrap();
        let second = load_or_create_artifact(&config).unwrap();

        assert_eq!(
            first.commit_sha, second.commit_sha,
            "loading twice must yield the same commit"
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn relative_repo_path_canonicalized_and_integrates_from_temp_workspace() {
        use crate::artifacts::{ArtifactUpdate, FileChange, create_workspace, integrate};

        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let rel = format!("target/forge-relative-test-{}-{seq}", std::process::id());
        let _ = std::fs::remove_dir_all(&rel);

        let config = ArtifactConfig {
            repo_path: rel.clone(),
            branch: "main".to_string(),
        };

        let artifact = load_or_create_artifact(&config).unwrap();

        assert!(
            artifact.repo_path.is_absolute(),
            "repo_path must be canonicalized to absolute"
        );

        let workspace_path =
            std::env::temp_dir().join(format!("forge-rel-workspace-{}-{seq}", std::process::id()));
        let mut workspace = create_workspace(&artifact, workspace_path.clone());

        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "result.txt".to_string(),
                content: "from relative repo\n".to_string(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();

        let integrated = integrate(&artifact, &workspace).unwrap();

        assert_ne!(
            integrated.commit_sha, artifact.commit_sha,
            "integration from temp workspace must produce a new commit"
        );

        let _ = std::fs::remove_dir_all(&rel);
        let _ = std::fs::remove_dir_all(&workspace_path);
    }

    #[test]
    fn runtime_creates_telemetry_directory() {
        let dir = temp_path("telemetry-dir");
        let _ = std::fs::remove_dir_all(&dir);

        let _sink = FileTelemetry::new(dir.clone());

        assert!(dir.exists(), "telemetry directory must be created");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Creates a bare repo with two branches and HEAD pointing to the non-default one:
    ///   main  -> Commit A  (contains a.txt)
    ///   other -> Commit B  (contains b.txt)
    ///   HEAD  -> other
    ///
    /// Returns (bare_repo_path, sha_on_main, sha_on_other).
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
        git(&["commit", "--quiet", "-m", "Commit A"]);
        let sha_main = sha(&["rev-parse", "HEAD"]);

        git(&["checkout", "--quiet", "-b", "other"]);
        std::fs::write(seed.join("b.txt"), "on other\n").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "--quiet", "-m", "Commit B"]);
        let sha_other = sha(&["rev-parse", "HEAD"]);

        // Clone bare with HEAD -> other (whatever the seed is currently on).
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
    fn load_existing_artifact_uses_configured_branch_not_head() {
        let base = temp_path("branch-not-head");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let (repo_path, sha_main, sha_other) = make_two_branch_bare_repo(&base);
        assert_ne!(sha_main, sha_other, "test requires two distinct commits");

        let config = ArtifactConfig {
            repo_path: repo_path.to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };

        let artifact = load_or_create_artifact(&config).unwrap();
        assert_eq!(
            artifact.commit_sha, sha_main,
            "must resolve configured branch (main), not bare repo HEAD (other)"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn runtime_uses_always_pass_when_validation_absent() {
        use crate::artifacts::Workspace;

        let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
        let validator = make_validator(None);
        let result = validator.validate(&ws);
        assert!(
            result.passed,
            "absent validation config must yield a passing validator"
        );
    }

    #[test]
    fn runtime_uses_command_validator_when_configured() {
        use crate::artifacts::Workspace;
        use crate::config::ValidationConfig;

        let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
        // A failing command proves the CommandValidator is active, not AlwaysPassValidator.
        let config = ValidationConfig {
            commands: vec!["false".to_string()],
            timeout_seconds: None,
        };
        let validator = make_validator(Some(&config));
        let result = validator.validate(&ws);
        assert!(
            !result.passed,
            "configured command validator must run commands and fail on non-zero exit"
        );
    }

    #[test]
    fn missing_configured_branch_returns_error() {
        let base = temp_path("missing-branch");
        let _ = std::fs::remove_dir_all(&base);

        // Create a repo whose only branch is "other".
        let config_other = ArtifactConfig {
            repo_path: base.join("artifact.git").to_str().unwrap().to_string(),
            branch: "other".to_string(),
        };
        load_or_create_artifact(&config_other).unwrap();

        // Now try to load with branch "main", which does not exist.
        let config_main = ArtifactConfig {
            repo_path: base.join("artifact.git").to_str().unwrap().to_string(),
            branch: "main".to_string(),
        };
        let result = load_or_create_artifact(&config_main);
        assert!(
            result.is_err(),
            "must fail when configured branch is absent"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("main"),
            "error must mention the missing branch name, got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn runtime_summary_uses_post_integration_artifact_commit() {
        use crate::artifacts::{ArtifactUpdate, FileChange};
        use crate::machines::scheduler::{
            NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler,
            SchedulerMachine, WorkOutput,
        };
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
        use crate::telemetry::NoopTelemetry;
        use std::fs;
        use std::process::Command;

        // Returns PlanAccepted for Plan nodes and WorkAccepted (with an
        // ArtifactUpdate) for Work nodes, so the full RunNode → IntegrateWork
        // path is exercised and the artifact commit actually advances.
        struct FileWritingRunner;
        impl NodeRunner for FileWritingRunner {
            fn run_node(
                &self,
                request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                match request.kind {
                    NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                        children: vec![NodeRequest {
                            id: NodeId("work".to_string()),
                            kind: NodeKind::Work,
                            objective: "generate result.txt".to_string(),
                            dependencies: vec![],
                        }],
                    }),
                    NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "wrote result.txt".to_string(),
                        },
                        artifact_update: Some(ArtifactUpdate {
                            changes: vec![FileChange::Write {
                                path: "result.txt".to_string(),
                                content: "generated\n".to_string(),
                            }],
                        }),
                    }),
                }
            }
        }

        let base = temp_path("post-integration");
        let _ = fs::remove_dir_all(&base);
        let seed = base.join("seed");
        fs::create_dir_all(&seed).unwrap();

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
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Runtime Test"]);
        git(&["config", "user.email", "runtime-test@example.invalid"]);
        fs::write(seed.join("seed.txt"), "initial\n").unwrap();
        git(&["add", "seed.txt"]);
        git(&["commit", "--quiet", "-m", "Initial"]);

        let repo_path = base.join("artifact.git");
        assert!(
            Command::new("git")
                .args(["clone", "--quiet", "--bare"])
                .arg(&seed)
                .arg(&repo_path)
                .status()
                .unwrap()
                .success(),
            "git clone --bare failed"
        );

        let initial_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo_path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        let artifact = Artifact {
            repo_path,
            branch: "main".to_string(),
            commit_sha: initial_sha.clone(),
        };

        let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact);
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "generate a file".to_string(),
        });
        let (_output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let final_artifact = handler
            .artifact()
            .expect("artifact must be present after run");
        assert_ne!(
            final_artifact.commit_sha, initial_sha,
            "runtime summary must use the post-integration artifact commit, not the initial one"
        );

        let _ = fs::remove_dir_all(&base);
    }

    // ── validation_passed manifest tests ─────────────────────────────────────

    /// Build a bare-repo artifact and return (base_dir, artifact, initial_sha).
    fn make_bare_artifact(label: &str) -> (PathBuf, Artifact, String) {
        use std::fs;
        use std::process::Command;

        let base = temp_path(label);
        let _ = fs::remove_dir_all(&base);
        let seed = base.join("seed");
        fs::create_dir_all(&seed).unwrap();

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
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Runtime Test"]);
        git(&["config", "user.email", "runtime-test@example.invalid"]);
        fs::write(seed.join("seed.txt"), "initial\n").unwrap();
        git(&["add", "seed.txt"]);
        git(&["commit", "--quiet", "-m", "Initial"]);

        let repo_path = base.join("artifact.git");
        assert!(
            Command::new("git")
                .args(["clone", "--quiet", "--bare"])
                .arg(&seed)
                .arg(&repo_path)
                .status()
                .unwrap()
                .success(),
            "git clone --bare failed"
        );

        let initial_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo_path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        let artifact = Artifact {
            repo_path,
            branch: "main".to_string(),
            commit_sha: initial_sha.clone(),
        };

        (base, artifact, initial_sha)
    }

    #[test]
    fn successful_validated_run_sets_validation_passed_true() {
        use crate::artifacts::{ArtifactUpdate, FileChange};
        use crate::machines::scheduler::{
            NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler,
            SchedulerMachine, WorkOutput,
        };
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
        use crate::runtime::{create_run, finalize_manifest};
        use crate::telemetry::NoopTelemetry;

        struct FileWritingRunner;
        impl NodeRunner for FileWritingRunner {
            fn run_node(
                &self,
                request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                match request.kind {
                    NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                        children: vec![NodeRequest {
                            id: NodeId("work".to_string()),
                            kind: NodeKind::Work,
                            objective: "write result.txt".to_string(),
                            dependencies: vec![],
                        }],
                    }),
                    NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "wrote result.txt".to_string(),
                        },
                        artifact_update: Some(ArtifactUpdate {
                            changes: vec![FileChange::Write {
                                path: "result.txt".to_string(),
                                content: "generated\n".to_string(),
                            }],
                        }),
                    }),
                }
            }
        }

        let (base, artifact, _) = make_bare_artifact("vp-manifest-true");
        let runs_root = base.join("runs");

        let run_info = create_run(&runs_root, "test", "repo", "provider").unwrap();
        let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact);
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "generate a file".to_string(),
        });
        let (output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let validation_passed = handler.validation_passed();
        let status = match &output {
            SchedulerOutput::Complete { .. } => "succeeded",
            SchedulerOutput::Failed { .. } => "failed",
        };
        finalize_manifest(&run_info, status, None, validation_passed, None).unwrap();

        let content = std::fs::read_to_string(run_info.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert_eq!(
            v["validation_passed"],
            serde_json::Value::Bool(true),
            "manifest must record validation_passed=true for a successful validated run"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn validation_failure_sets_validation_passed_false_in_manifest() {
        use crate::artifacts::Workspace;
        use crate::artifacts::{ArtifactUpdate, FileChange};
        use crate::machines::scheduler::{
            NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler,
            SchedulerMachine, WorkOutput,
        };
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
        use crate::runtime::{create_run, finalize_manifest};
        use crate::telemetry::NoopTelemetry;
        use crate::validation::{ValidationResult, Validator};

        struct FileWritingRunner;
        impl NodeRunner for FileWritingRunner {
            fn run_node(
                &self,
                request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                match request.kind {
                    NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                        children: vec![NodeRequest {
                            id: NodeId("work".to_string()),
                            kind: NodeKind::Work,
                            objective: "write result.txt".to_string(),
                            dependencies: vec![],
                        }],
                    }),
                    NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "wrote result.txt".to_string(),
                        },
                        artifact_update: Some(ArtifactUpdate {
                            changes: vec![FileChange::Write {
                                path: "result.txt".to_string(),
                                content: "generated\n".to_string(),
                            }],
                        }),
                    }),
                }
            }
        }

        struct AlwaysFailValidator;
        impl Validator for AlwaysFailValidator {
            fn validate(&self, _workspace: &Workspace) -> ValidationResult {
                ValidationResult {
                    passed: false,
                    summary: "intentional failure".to_string(),
                }
            }
        }

        let (base, artifact, _) = make_bare_artifact("vp-manifest-false");
        let runs_root = base.join("runs");

        let run_info = create_run(&runs_root, "test", "repo", "provider").unwrap();
        let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact)
            .with_validator(Rc::new(AlwaysFailValidator));
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "generate a file".to_string(),
        });
        let (output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let validation_passed = handler.validation_passed();
        let status = match &output {
            SchedulerOutput::Complete { .. } => "succeeded",
            SchedulerOutput::Failed { .. } => "failed",
        };
        finalize_manifest(&run_info, status, None, validation_passed, None).unwrap();

        let content = std::fs::read_to_string(run_info.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(
            v["validation_passed"],
            serde_json::Value::Bool(false),
            "manifest must record validation_passed=false when validator rejects"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn failed_manifest_contains_concrete_failure_reason() {
        use crate::machines::scheduler::{RunRequest, SchedulerHandler, SchedulerMachine};
        use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunner};
        use crate::runtime::{create_run, finalize_manifest};
        use crate::telemetry::NoopTelemetry;

        struct AlwaysFailRunner;
        impl NodeRunner for AlwaysFailRunner {
            fn run_node(
                &self,
                _request: NodeRunRequest,
                _telemetry: &dyn crate::telemetry::TelemetrySink,
            ) -> NodeRunResult {
                use crate::machines::scheduler::event::{NodeFailure, RecoveryAction};
                NodeRunResult::Failed(NodeFailure {
                    reason: "provider error (Retryable): connection refused".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "deliberation failed".to_string(),
                    },
                })
            }
        }

        let (base, artifact, _) = make_bare_artifact("vp-manifest-null");
        let runs_root = base.join("runs");

        let run_info = create_run(&runs_root, "test", "repo", "provider").unwrap();
        let handler = SchedulerHandler::with_artifact(AlwaysFailRunner, artifact);
        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: "do something".to_string(),
        });
        let (output, handler) = run_machine_with_telemetry(handler, initial_state, &NoopTelemetry);

        let validation_passed = handler.validation_passed();
        let (status, failure_reason) = match &output {
            SchedulerOutput::Complete { .. } => ("succeeded", None),
            SchedulerOutput::Failed { reason, .. } => ("failed", Some(reason.as_str())),
        };
        finalize_manifest(&run_info, status, None, validation_passed, failure_reason).unwrap();

        let content = std::fs::read_to_string(run_info.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(
            v["validation_passed"],
            serde_json::Value::Null,
            "manifest must record validation_passed=null when failure occurs before validation"
        );
        assert!(
            v["failure_reason"]
                .as_str()
                .unwrap()
                .contains("provider error (Retryable): connection refused"),
            "manifest must record the concrete provider failure reason"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
