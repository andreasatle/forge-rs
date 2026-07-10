use super::*;
use crate::artifacts::Artifact;
use crate::config::{
    ArtifactConfig, ProviderBackend, ProviderConfig, ProviderTierConfig, UnmanagedProviderConfig,
};
use crate::machines::scheduler::{SchedulerTerminalOutput, run_scheduler_with_telemetry};
use crate::runtime::ProviderRunMetadata;
use crate::runtime::provider_stack::ResolvedProviderStack;
use crate::runtime::repo::load_or_create_artifact;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temp_path(label: &str) -> PathBuf {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "forge-runtime-test-{label}-{}-{seq}",
        std::process::id()
    ))
}

fn artifact_config(path: &Path) -> ArtifactConfig {
    ArtifactConfig {
        repo_path: path.to_str().unwrap().to_string(),
        branch: "main".to_string(),
    }
}

fn test_provider_metadata() -> ProviderRunMetadata {
    ResolvedProviderStack::build(&unmanaged_provider("provider", "llama-test", 512))
        .expect("test provider stack must build")
        .metadata
}

fn unmanaged_provider(base_url: &str, model: &str, n_predict: usize) -> ProviderConfig {
    ProviderConfig {
        cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
            base_url: base_url.to_string(),
            model: model.to_string(),
            n_predict,
            backend: ProviderBackend::LlamaCpp,
        }),
        strong: None,
        timeout_seconds: 120,
        strong_timeout_seconds: None,
    }
}

#[test]
fn load_or_create_artifact_creates_missing_repo() {
    let path = temp_path("create-missing");
    let _ = std::fs::remove_dir_all(&path);

    let config = artifact_config(&path);
    let result = load_or_create_artifact(&config, None);

    assert!(result.is_ok(), "expected artifact creation to succeed");
    assert!(path.exists(), "bare repo directory must be created");

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn load_or_create_artifact_sets_branch() {
    let path = temp_path("branch");
    let _ = std::fs::remove_dir_all(&path);

    let config = artifact_config(&path);
    let artifact = load_or_create_artifact(&config, None).unwrap();

    assert_eq!(artifact.branch, "main");

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn load_or_create_artifact_loads_existing_repo() {
    let path = temp_path("load-existing");
    let _ = std::fs::remove_dir_all(&path);

    let config = artifact_config(&path);
    let first = load_or_create_artifact(&config, None).unwrap();
    let second = load_or_create_artifact(&config, None).unwrap();

    assert_eq!(
        first.commit_sha, second.commit_sha,
        "loading twice must yield the same commit"
    );

    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn relative_repo_path_canonicalized_and_integrates_from_temp_workspace() {
    use crate::artifacts::{WorkspaceFactory, WorkspaceFileOps, integrate};

    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let rel = format!("target/forge-relative-test-{}-{seq}", std::process::id());
    let _ = std::fs::remove_dir_all(&rel);

    let config = ArtifactConfig {
        repo_path: rel.clone(),
        branch: "main".to_string(),
    };

    let artifact = load_or_create_artifact(&config, None).unwrap();

    assert!(
        artifact.repo_path.is_absolute(),
        "repo_path must be canonicalized to absolute"
    );

    let workspace_path =
        std::env::temp_dir().join(format!("forge-rel-workspace-{}-{seq}", std::process::id()));
    let mut workspace = WorkspaceFactory::new(&artifact).create_workspace(workspace_path.clone());

    workspace
        .write_file("result.txt", "from relative repo\n")
        .unwrap();

    let integrated = integrate(&artifact, &workspace).unwrap();

    assert_ne!(
        integrated.commit_sha, artifact.commit_sha,
        "integration from temp workspace must produce a new commit"
    );

    let _ = std::fs::remove_dir_all(&rel);
    let _ = std::fs::remove_dir_all(&workspace_path);
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
            crate::git::command()
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
            crate::git::command()
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
        crate::git::command()
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

    let artifact = load_or_create_artifact(&config, None).unwrap();
    assert_eq!(
        artifact.commit_sha, sha_main,
        "must resolve configured branch (main), not bare repo HEAD (other)"
    );

    let _ = std::fs::remove_dir_all(&base);
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
    load_or_create_artifact(&config_other, None).unwrap();

    // Now try to load with branch "main", which does not exist.
    let config_main = ArtifactConfig {
        repo_path: base.join("artifact.git").to_str().unwrap().to_string(),
        branch: "main".to_string(),
    };
    let result = load_or_create_artifact(&config_main, None);
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
    use crate::artifacts::WorkspaceFileOps;
    use crate::machines::scheduler::{
        NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler, SchedulerMachine,
        WorkOutput,
    };
    use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
    use crate::telemetry::NoopTelemetry;
    use std::fs;

    // Returns PlanAccepted for Plan nodes and mutates the WorkAttempt
    // workspace for Work nodes, so the full RunNode → IntegrateWork path
    // is exercised and the artifact commit actually advances.
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
                        worker_role: None,
                        objective: "generate result.txt".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    }],
                    tasks: vec![],
                }),
                NodeKind::Work => {
                    request
                        .work_attempt
                        .expect("artifact Work must receive a WorkAttempt")
                        .workspace
                        .borrow_mut()
                        .write_file("result.txt", "generated\n")
                        .expect("test runner must write result.txt");
                    NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "wrote result.txt".to_string(),
                        },
                    })
                }
            }
        }
    }

    let base = temp_path("post-integration");
    let _ = fs::remove_dir_all(&base);
    let seed = base.join("seed");
    fs::create_dir_all(&seed).unwrap();

    let git = |args: &[&str]| {
        assert!(
            crate::git::command()
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
        crate::git::command()
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&repo_path)
            .status()
            .unwrap()
            .success(),
        "git clone --bare failed"
    );

    let initial_sha = String::from_utf8(
        crate::git::command()
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
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "generate a file".to_string(),
        },
        RunConfig::default(),
    );
    let (_output, handler) = run_scheduler_with_telemetry(handler, initial_state, &NoopTelemetry);

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

    let base = temp_path(label);
    let _ = fs::remove_dir_all(&base);
    let seed = base.join("seed");
    fs::create_dir_all(&seed).unwrap();

    let git = |args: &[&str]| {
        assert!(
            crate::git::command()
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
        crate::git::command()
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&repo_path)
            .status()
            .unwrap()
            .success(),
        "git clone --bare failed"
    );

    let initial_sha = String::from_utf8(
        crate::git::command()
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
    use crate::artifacts::WorkspaceFileOps;
    use crate::machines::scheduler::{
        NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler, SchedulerMachine,
        WorkOutput,
    };
    use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
    use crate::runtime::create_run;
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
                        worker_role: None,
                        objective: "write result.txt".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    }],
                    tasks: vec![],
                }),
                NodeKind::Work => {
                    request
                        .work_attempt
                        .expect("artifact Work must receive a WorkAttempt")
                        .workspace
                        .borrow_mut()
                        .write_file("result.txt", "generated\n")
                        .expect("test runner must write result.txt");
                    NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "wrote result.txt".to_string(),
                        },
                    })
                }
            }
        }
    }

    let (base, artifact, _) = make_bare_artifact("vp-manifest-true");
    let runs_root = base.join("runs");

    let run_info = create_run(&runs_root, "test", "repo", &test_provider_metadata()).unwrap();
    let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact);
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "generate a file".to_string(),
        },
        RunConfig::default(),
    );
    let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, &NoopTelemetry);

    let validation_passed = handler.validation_passed();
    let status = match &output {
        SchedulerTerminalOutput::Complete { .. } => "succeeded",
        SchedulerTerminalOutput::Failed { .. } => "failed",
    };
    run_info
        .finalize_manifest(status, None, validation_passed, None)
        .unwrap();

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
    use crate::artifacts::{Workspace, WorkspaceFileOps};
    use crate::machines::scheduler::{
        NodeId, NodeKind, NodeRequest, PlanOutput, RunRequest, SchedulerHandler, SchedulerMachine,
        WorkOutput,
    };
    use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunWorkResult, NodeRunner};
    use crate::runtime::create_run;
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
                        worker_role: None,
                        objective: "write result.txt".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    }],
                    tasks: vec![],
                }),
                NodeKind::Work => {
                    request
                        .work_attempt
                        .expect("artifact Work must receive a WorkAttempt")
                        .workspace
                        .borrow_mut()
                        .write_file("result.txt", "generated\n")
                        .expect("test runner must write result.txt");
                    NodeRunResult::WorkAccepted(NodeRunWorkResult {
                        work: WorkOutput {
                            summary: "wrote result.txt".to_string(),
                        },
                    })
                }
            }
        }
    }

    struct AlwaysFailValidator;
    impl Validator for AlwaysFailValidator {
        fn validate(&self, _workspace: &Workspace) -> ValidationResult {
            ValidationResult {
                passed: false,
                summary: "intentional failure".to_string(),
                failure: None,
            }
        }
    }

    let (base, artifact, _) = make_bare_artifact("vp-manifest-false");
    let runs_root = base.join("runs");

    let run_info = create_run(&runs_root, "test", "repo", &test_provider_metadata()).unwrap();
    let handler = SchedulerHandler::with_artifact(FileWritingRunner, artifact)
        .with_validator(Rc::new(AlwaysFailValidator));
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "generate a file".to_string(),
        },
        RunConfig::default(),
    );
    let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, &NoopTelemetry);

    let validation_passed = handler.validation_passed();
    let status = match &output {
        SchedulerTerminalOutput::Complete { .. } => "succeeded",
        SchedulerTerminalOutput::Failed { .. } => "failed",
    };
    run_info
        .finalize_manifest(status, None, validation_passed, None)
        .unwrap();

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
    use crate::runtime::create_run;
    use crate::telemetry::NoopTelemetry;

    struct AlwaysFailRunner;
    impl NodeRunner for AlwaysFailRunner {
        fn run_node(
            &self,
            _request: NodeRunRequest,
            _telemetry: &dyn crate::telemetry::TelemetrySink,
        ) -> NodeRunResult {
            use crate::machines::scheduler::{FailureKind, NodeFailure, RecoveryAction};
            NodeRunResult::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "provider error (Retryable): connection refused".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "deliberation failed".to_string(),
                },
            })
        }
    }

    let (base, artifact, _) = make_bare_artifact("vp-manifest-null");
    let runs_root = base.join("runs");

    let run_info = create_run(&runs_root, "test", "repo", &test_provider_metadata()).unwrap();
    let handler = SchedulerHandler::with_artifact(AlwaysFailRunner, artifact);
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "do something".to_string(),
        },
        RunConfig::default(),
    );
    let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, &NoopTelemetry);

    let validation_passed = handler.validation_passed();
    let failure_reason_str: Option<String> =
        if let SchedulerTerminalOutput::Failed { reason, .. } = &output {
            Some(reason.to_string())
        } else {
            None
        };
    let status = if matches!(output, SchedulerTerminalOutput::Failed { .. }) {
        "failed"
    } else {
        "succeeded"
    };
    run_info
        .finalize_manifest(
            status,
            None,
            validation_passed,
            failure_reason_str.as_deref(),
        )
        .unwrap();

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

// ── RunConfig derivation ─────────────────────────────────────────────────

#[test]
fn run_config_has_strong_tier_false_when_provider_strong_is_none() {
    use crate::machines::scheduler::{RunRequest, SchedulerMachine};

    let provider = unmanaged_provider("http://localhost:8080", "cheap", 512);
    assert!(
        provider.strong.is_none(),
        "test requires no strong tier configured"
    );

    let run_config = RunConfig {
        has_strong_tier: provider.strong.is_some(),
    };
    let state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "test".to_string(),
        },
        run_config,
    );

    let embedded = match &state {
        SchedulerState::Active { run_config, .. } => run_config,
        _ => panic!("initial_state must return Active"),
    };
    assert!(
        !embedded.has_strong_tier,
        "has_strong_tier must be false when provider.strong is None; \
         RunConfig::default() would give true, causing silent retry on identical model"
    );
}

#[test]
fn run_config_has_strong_tier_true_when_provider_strong_is_some() {
    use crate::machines::scheduler::{RunRequest, SchedulerMachine};

    let provider = ProviderConfig {
        cheap: ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
            base_url: "http://localhost:8080".to_string(),
            model: "cheap".to_string(),
            n_predict: 512,
            backend: ProviderBackend::LlamaCpp,
        }),
        strong: Some(ProviderTierConfig::Unmanaged(UnmanagedProviderConfig {
            base_url: "http://localhost:8081".to_string(),
            model: "strong".to_string(),
            n_predict: 1024,
            backend: ProviderBackend::LlamaCpp,
        })),
        timeout_seconds: 120,
        strong_timeout_seconds: None,
    };

    let run_config = RunConfig {
        has_strong_tier: provider.strong.is_some(),
    };
    let state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "test".to_string(),
        },
        run_config,
    );

    let embedded = match &state {
        SchedulerState::Active { run_config, .. } => run_config,
        _ => panic!("initial_state must return Active"),
    };
    assert!(
        embedded.has_strong_tier,
        "has_strong_tier must be true when provider.strong is Some"
    );
}
