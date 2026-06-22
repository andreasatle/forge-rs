//! Forge runtime — wires config into machines and drives a single run.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static SEED_COUNTER: AtomicU64 = AtomicU64::new(0);

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::{ArtifactConfig, ForgeConfig};
use crate::engine::run_machine_with_telemetry;
use crate::machines::scheduler::{RunRequest, SchedulerHandler, SchedulerMachine, SchedulerOutput};
use crate::node_runner::DeliberatingNodeRunner;
use crate::providers::{
    LlamaCppProvider, ProviderClient, ProviderError, ProviderRequest, ProviderResponse,
    RetryingProvider,
};
use crate::telemetry::{FileTelemetry, TelemetrySink};

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
        self.inner.call(ProviderRequest { prompt: wrapped })
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

        let telemetry_dir = PathBuf::from(&config.telemetry.directory);
        let _ = std::fs::remove_dir_all(&telemetry_dir);
        let sink: Rc<dyn TelemetrySink> = Rc::new(FileTelemetry::new(telemetry_dir.clone())?);

        let llama = LlamaCppProvider::new(&config.provider.base_url)
            .with_n_predict(config.provider.n_predict as u32);
        let retrying = RetryingProvider::new(llama, 3);
        let instructed = InstructedProvider { inner: retrying };

        let runner = DeliberatingNodeRunner::new(instructed);
        let handler =
            SchedulerHandler::with_artifact(runner, artifact).with_telemetry(Rc::clone(&sink));

        let initial_state = SchedulerMachine::initial_state(RunRequest {
            objective: config.objective.clone(),
        });

        let (output, handler) = run_machine_with_telemetry(handler, initial_state, sink.as_ref());

        let final_artifact = handler.artifact();
        print_summary(&output, &config, final_artifact.as_ref(), &telemetry_dir);

        Ok(())
    }
}

/// Load the artifact at `config.repo_path`, creating a bare repo if it does not exist.
pub fn load_or_create_artifact(config: &ArtifactConfig) -> Result<Artifact, Box<dyn Error>> {
    let repo_path = PathBuf::from(&config.repo_path);

    if !repo_path.exists() {
        create_bare_repo(&repo_path, &config.branch)?;
    }

    let repo_path = repo_path.canonicalize()?;
    let commit_sha = git_head(&repo_path)?;

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

fn git_head(repo_path: &Path) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()?;
    if !output.status.success() {
        return Err("git rev-parse HEAD failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn print_summary(
    output: &SchedulerOutput,
    config: &ForgeConfig,
    artifact: Option<&Artifact>,
    telemetry_dir: &Path,
) {
    let result_str = match output {
        SchedulerOutput::Complete { .. } => "COMPLETE",
        SchedulerOutput::Failed { .. } => "FAILED",
    };

    println!("Result      : {result_str}");
    println!("Artifact repo: {}", config.artifact.repo_path);

    if let Some(a) = artifact {
        let short_sha = &a.commit_sha[..a.commit_sha.len().min(7)];
        println!("Commit      : {short_sha}");
        println!("Telemetry   : {}", telemetry_dir.display());

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
        println!("Telemetry   : {}", telemetry_dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ArtifactConfig;
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

        let integrated = integrate(&artifact, &workspace);

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

        let _sink = FileTelemetry::new(dir.clone()).unwrap();

        assert!(dir.exists(), "telemetry directory must be created");

        let _ = std::fs::remove_dir_all(&dir);
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
