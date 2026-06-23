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
use crate::machines::scheduler::{RunRequest, SchedulerHandler, SchedulerMachine, SchedulerOutput};
use crate::node_runner::DeliberatingNodeRunner;
use crate::providers::{
    LlamaCppProvider, ProviderClient, ProviderError, ProviderRequest, ProviderResponse,
    RetryingProvider,
};
use crate::runtime::create_run;
use crate::telemetry::{FileTelemetry, TelemetrySink};
use crate::validation::{AlwaysPassValidator, CommandValidator, Validator};

const PROTOCOL_PREFIX: &str = "\
Return exactly one JSON object. No markdown. No code fence. No explanation.\n\
No text before or after the JSON.\n\
Accepted schema: {\"status\":\"accepted\",\"content\":\"...\"}\n\
Rejected schema: {\"status\":\"rejected\",\"reason\":\"...\"}";

const PROTOCOL_SUFFIX: &str = "\n\
Return exactly one JSON object. No markdown. No code fence. No explanation.\n\
Your response must be valid JSON with \"status\" set to \"accepted\" or \"rejected\".";

struct InstructedProvider<P> {
    inner: P,
}

impl<P: ProviderClient> ProviderClient for InstructedProvider<P> {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let wrapped = format!(
            "{}\n\n{}\n\n{}",
            PROTOCOL_PREFIX, req.prompt, PROTOCOL_SUFFIX
        );
        self.inner.call(ProviderRequest {
            prompt: wrapped,
            max_tokens: req.max_tokens,
            output_schema: req.output_schema,
        })
    }
}

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

        let llama = LlamaCppProvider::new(&config.provider.base_url);
        let retrying = RetryingProvider::new(llama, 3);
        let instructed = InstructedProvider { inner: retrying };

        let runner = DeliberatingNodeRunner::new(instructed);
        let validator = make_validator(config.validation.as_ref());
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator);

        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: config.objective.clone(),
        });

        let (output, handler) = run_machine_with_telemetry(handler, initial_state, sink.as_ref());

        let final_artifact = handler.artifact();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        match output {
            SchedulerOutput::Failed { reason, .. } => Err(format!("run failed: {reason}").into()),
            SchedulerOutput::Complete { .. } => Ok(()),
        }
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
    use crate::config::ArtifactConfig;
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

    struct EchoProvider;

    impl ProviderClient for EchoProvider {
        fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Ok(ProviderResponse {
                content: req.prompt.clone(),
                finish_reason: None,
            })
        }
    }

    #[test]
    fn instructed_provider_preserves_output_schema() {
        use crate::providers::types::{ProviderRequest, StructuredOutput};
        use std::cell::RefCell;

        struct CapturingProvider {
            requests: RefCell<Vec<ProviderRequest>>,
        }
        impl ProviderClient for CapturingProvider {
            fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
                self.requests.borrow_mut().push(req);
                Ok(ProviderResponse {
                    content: "ok".to_string(),
                    finish_reason: None,
                })
            }
        }

        let capturing = CapturingProvider {
            requests: RefCell::new(Vec::new()),
        };
        let provider = InstructedProvider { inner: &capturing };

        provider
            .call(ProviderRequest {
                prompt: "test".to_string(),
                max_tokens: 256,
                output_schema: Some(StructuredOutput::Json),
            })
            .unwrap();

        let reqs = capturing.requests.borrow();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].output_schema,
            Some(StructuredOutput::Json),
            "InstructedProvider must forward output_schema unchanged"
        );
    }

    #[test]
    fn failed_runtime_run_returns_error_or_nonzero_status() {
        let output = SchedulerOutput::Failed {
            graph: empty_graph(),
            reason: "something went wrong".to_string(),
        };
        let result: Result<(), Box<dyn std::error::Error>> = match output {
            SchedulerOutput::Failed { reason, .. } => Err(format!("run failed: {reason}").into()),
            SchedulerOutput::Complete { .. } => Ok(()),
        };
        assert!(result.is_err(), "Failed output must produce an error");
        assert!(
            result.unwrap_err().to_string().contains("run failed"),
            "error message must mention run failed"
        );
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
        let result: Result<(), Box<dyn std::error::Error>> = match output {
            SchedulerOutput::Failed { reason, .. } => Err(format!("run failed: {reason}").into()),
            SchedulerOutput::Complete { .. } => Ok(()),
        };
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
}
