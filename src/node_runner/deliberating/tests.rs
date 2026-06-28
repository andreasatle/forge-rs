use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::artifacts::{ArtifactView, FileChange};
use crate::machines::scheduler::{FailureKind, ModelTier, NodeId, NodeKind, RecoveryAction};
use crate::providers::{ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse};
use crate::telemetry::NoopTelemetry;

/// Provider that records the `max_tokens` from the first request it receives.
struct CapturingProvider {
    max_tokens: RefCell<Option<u32>>,
    responses: RefCell<VecDeque<String>>,
}

impl CapturingProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            max_tokens: RefCell::new(None),
            responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn captured_max_tokens(&self) -> Option<u32> {
        *self.max_tokens.borrow()
    }
}

impl ProviderClient for CapturingProvider {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        if self.max_tokens.borrow().is_none() {
            *self.max_tokens.borrow_mut() = Some(req.max_tokens);
        }
        let content = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("CapturingProvider: responses exhausted");
        Ok(ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

struct ScriptedProvider {
    responses: RefCell<VecDeque<Result<String, ProviderError>>>,
}

impl ScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            responses: RefCell::new(responses.iter().map(|s| Ok(s.to_string())).collect()),
        }
    }

    fn failing(kind: ProviderErrorKind, message: &str) -> Self {
        Self {
            responses: RefCell::new(VecDeque::from([Err(ProviderError {
                kind,
                message: message.to_string(),
            })])),
        }
    }
}

impl ProviderClient for ScriptedProvider {
    fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("ScriptedProvider: responses exhausted")
            .map(|content| ProviderResponse {
                content,
                finish_reason: None,
            })
    }
}

/// Provider that records every prompt it receives, then returns a fixed response.
struct RecordingProvider {
    prompts: RefCell<Vec<String>>,
    responses: RefCell<VecDeque<String>>,
}

impl RecordingProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            prompts: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn recorded_prompts(&self) -> Vec<String> {
        self.prompts.borrow().clone()
    }
}

impl ProviderClient for RecordingProvider {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.prompts.borrow_mut().push(req.prompt.clone());
        let content = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("RecordingProvider: responses exhausted");
        Ok(ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

fn plan_request(objective: &str) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Plan,
        objective: objective.to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
    }
}

fn work_request(objective: &str) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
    }
}

fn strong_work_request(objective: &str) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        model_tier: ModelTier::Strong,
        attempt: 0,
        artifact_view: None,
    }
}

// --- git helpers for artifact_view tests ---

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let seq = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-deliberating-{label}-{}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self(path)
    }

    fn join(&self, s: &str) -> PathBuf {
        self.0.join(s)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .expect("failed to run git");
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn git_output(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("failed to run git");
    assert!(out.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Creates a bare git repo with a single committed file and returns an ArtifactView.
fn make_artifact_view(temp: &TempDir, filename: &str, content: &str) -> ArtifactView {
    let seed = temp.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    fs::write(seed.join(filename), content).unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.join("artifact.git");
    let status = Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare failed");
    assert!(status.success());
    let commit_sha = git_output(&bare, &["rev-parse", "HEAD"]);
    ArtifactView {
        repo_path: bare,
        commit_sha,
    }
}

fn work_request_with_artifact(objective: &str, temp: &TempDir) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(make_artifact_view(temp, "hello.txt", "world\n")),
    }
}

fn strong_work_request_with_artifact(objective: &str, temp: &TempDir) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        model_tier: ModelTier::Strong,
        attempt: 0,
        artifact_view: Some(make_artifact_view(temp, "hello.txt", "world\n")),
    }
}

// --- existing tests (updated for new WorkAccepted shape) ---

#[test]
fn deliberating_runner_plan_returns_plan_output() {
    let tasks_json = r#"{"tasks":[{"id":"task-1","objective":"the actual work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(plan_request("plan the work"), &NoopTelemetry);
    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted");
    };
    assert_eq!(plan.children.len(), 1);
    assert_eq!(plan.children[0].kind, NodeKind::Work);
    assert_eq!(plan.children[0].objective, "the actual work");
    assert_eq!(plan.children[0].target_files, vec!["work.txt".to_string()]);
}

#[test]
fn deliberating_runner_work_returns_work_output() {
    let temp = TempDir::new("work-output");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"finished\n"}"#,
        r#"{"status":"accepted","content":"finished the task"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        work_request_with_artifact("write some code", &temp),
        &NoopTelemetry,
    );
    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(work_result.work.summary, "finished the task");
}

#[test]
fn deliberating_runner_provider_failure_returns_failed() {
    let provider = ScriptedProvider::failing(
        ProviderErrorKind::Retryable,
        "connection refused on http://localhost:8080/completion",
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("do something"), &NoopTelemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        failure
            .message
            .contains("provider error (Retryable): connection refused")
    );
    assert!(matches!(failure.recovery, RecoveryAction::Retry { .. }));
}

#[test]
fn deliberating_runner_preserves_deliberation_failure_reason() {
    let provider = ScriptedProvider::failing(
        ProviderErrorKind::Retryable,
        "connection refused on http://localhost:8080/completion",
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("do something"), &NoopTelemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        failure
            .message
            .contains("provider error (Retryable): connection refused")
    );
    let RecoveryAction::Retry { message } = failure.recovery else {
        panic!("expected retry recovery for retryable provider error");
    };
    assert!(
        message.contains("provider error (Retryable): connection refused"),
        "retry message must include the original reason; got: {message}"
    );
}

#[test]
fn deliberating_runner_revision_uses_latest_producer_content() {
    let temp = TempDir::new("revision-latest");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"status":"accepted","content":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review done"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"needs work"}"#,
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"status":"accepted","content":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        work_request_with_artifact("refine the plan", &temp),
        &NoopTelemetry,
    );
    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(work_result.work.summary, "draft v2");
}

#[test]
fn deliberating_runner_preserves_deliberation_failure() {
    let provider = ScriptedProvider::from_strs(&[
        "not valid json at all",
        "still not valid json",
        "also not valid json",
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("do something"), &NoopTelemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(matches!(failure.recovery, RecoveryAction::Retry { .. }));
}

// --- new tests ---

#[test]
fn non_artifact_worker_without_tool_update_succeeds_without_artifact_update() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"output content"}"#,
        r#"{"status":"accepted","content":"output content"}"#,
        r#"{"status":"accepted","content":"output content"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("produce some output"), &NoopTelemetry);
    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(work_result.work.summary, "output content");
    assert!(
        work_result.artifact_update.is_none(),
        "explicit non-artifact Work must not synthesize an artifact update"
    );
}

#[test]
fn artifact_worker_without_tool_update_fails_semantic_validation() {
    let temp = TempDir::new("artifact-work-missing-update");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"summary of the work done"}"#,
        r#"{"status":"accepted","content":"summary of the work done"}"#,
        r#"{"status":"accepted","content":"summary of the work done"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        work_request_with_artifact("do some work", &temp),
        &NoopTelemetry,
    );
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(matches!(
        failure.kind,
        FailureKind::WorkSemanticValidationFailure
    ));
}

#[test]
fn artifact_view_context_is_visible_to_deliberation_prompt() {
    let temp = TempDir::new("prompt-context");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    // Critic and Referee are Work reviewers and must call read_file before
    // accepting.  Add read_file("hello.txt") calls for each reviewer.
    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do the thing".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("hello.txt"),
        "first prompt must list artifact files; got:\n{first}"
    );
    assert!(
        first.contains("do the thing"),
        "first prompt must include the original objective; got:\n{first}"
    );
}

#[test]
fn context_file_content_is_included_in_prompt_when_present() {
    let temp = TempDir::new("context-file-prompt");
    let view = make_artifact_view(&temp, "README.md", "This is the README.\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"README.md"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"README.md"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_context_file_names(vec!["README.md".to_string()]);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do the thing".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("This is the README."),
        "first prompt must include the README.md content; got:\n{first}"
    );
    assert!(
        first.contains("README.md"),
        "first prompt must name the context file; got:\n{first}"
    );
}

#[test]
fn absent_context_file_is_silently_omitted_from_prompt() {
    let temp = TempDir::new("context-file-absent");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    // Ask for README.md which does not exist in this artifact.
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_context_file_names(vec!["README.md".to_string()]);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do something".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("hello.txt"),
        "first prompt must still list artifact files; got:\n{first}"
    );
}

#[test]
fn no_context_file_names_produces_no_extra_content() {
    let temp = TempDir::new("no-context-files");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do something".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    let first = &prompts[0];
    assert!(
        first.contains("hello.txt"),
        "first prompt must list artifact files; got:\n{first}"
    );
    assert!(
        !first.contains("README.md"),
        "first prompt must not mention README.md when no context files configured; got:\n{first}"
    );
}

#[test]
fn deliberating_runner_threads_max_tokens_to_provider() {
    let provider = CapturingProvider::from_strs(&[
        r#"{"status":"accepted","content":"task completed"}"#,
        r#"{"status":"accepted","content":"review done"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_cheap_max_tokens(256);
    runner.run_node(work_request("test threading"), &NoopTelemetry);

    assert_eq!(
        provider.captured_max_tokens(),
        Some(256),
        "with_cheap_max_tokens must propagate to the provider request"
    );
}

#[test]
fn deliberating_work_result_includes_tool_artifact_update() {
    let temp = TempDir::new("tool-artifact-update");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    // Producer: first call returns write_file, second returns accepted.
    // Critic and Referee must call read_file before accepting (enforcement).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        r#"{"status":"accepted","content":"I wrote result.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "write a result file".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(
        work_result.work.summary, "I wrote result.txt",
        "summary must be the accepted content, not the tool request"
    );
    let update = work_result
        .artifact_update
        .expect("tool write_file must produce an artifact_update");
    assert_eq!(
        update.changes.len(),
        1,
        "must have exactly one pending change"
    );
    match &update.changes[0] {
        FileChange::Write { path, content } => {
            assert_eq!(path, "result.txt");
            assert_eq!(content, "done");
        }
        other => panic!("expected Write change from tool, got {other:?}"),
    }
}

// --- recovery classification runtime tests ---

#[test]
fn retryable_failure_produces_retry_action() {
    let provider = ScriptedProvider::failing(
        ProviderErrorKind::Retryable,
        "connection refused on http://localhost:8080/completion",
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(work_request("do something"), &telemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::Retry { .. }),
        "retryable provider error must produce Retry recovery"
    );
    let records = telemetry.into_records();
    let classified = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FailureClassified { .. }
        )
    });
    assert!(
        classified.is_some(),
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "Retry");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn semantic_failure_produces_elevate_action() {
    // Revision limit exhaustion is a semantic failure. The runner allows 1 revision,
    // so both Referee rejections are needed to exhaust the budget and produce
    // "revision limit exhausted: ..." → ElevateModel.
    let temp = TempDir::new("semantic-elevate");
    let provider = ScriptedProvider::from_strs(&[
        // Round 1: Producer → Critic → Referee rejects → revision loop.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"status":"accepted","content":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"needs improvement"}"#,
        // Round 2: Producer → Critic → Referee rejects → budget exhausted.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"status":"accepted","content":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"still not good enough"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        work_request_with_artifact("do something", &temp),
        &telemetry,
    );
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed; got success or plan");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::ElevateModel { .. }),
        "semantic failure must produce ElevateModel recovery; got {:?}",
        failure.recovery
    );
    let records = telemetry.into_records();
    let classified = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FailureClassified { .. }
        )
    });
    assert!(
        classified.is_some(),
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "ElevateModel");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn terminal_failure_produces_terminal_action() {
    let provider = ScriptedProvider::failing(ProviderErrorKind::Terminal, "invalid api key");
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(work_request("do something"), &telemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::Terminal { .. }),
        "fatal auth failure must produce Terminal recovery"
    );
    let records = telemetry.into_records();
    let classified = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FailureClassified { .. }
        )
    });
    assert!(
        classified.is_some(),
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "Terminal");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn deliberation_failure_produces_elevate_action_independent_of_message_text() {
    // Referee rejects with "task too large" twice, exhausting the revision budget.
    // The failure reason is "revision limit exhausted: task too large".
    // The classifier checks task-shape signals (Split) before revision-exhaustion
    // (ElevateModel), so "task too large" wins and maps to Split.
    let temp = TempDir::new("deliberation-elevate");
    let provider = ScriptedProvider::from_strs(&[
        // Round 1: Producer → Critic → Referee rejects "task too large" → revision loop.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"status":"accepted","content":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"task too large"}"#,
        // Round 2: Producer → Critic → Referee rejects "task too large" → budget exhausted.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"status":"accepted","content":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"task too large"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        work_request_with_artifact("do something", &temp),
        &telemetry,
    );
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::ElevateModel { .. }),
        "typed deliberation failure must produce ElevateModel recovery; got {:?}",
        failure.recovery
    );
    let records = telemetry.into_records();
    let classified = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FailureClassified { .. }
        )
    });
    assert!(
        classified.is_some(),
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "ElevateModel");
            }
            _ => unreachable!(),
        }
    }
}

// --- planner output tests ---

#[test]
fn prose_planner_content_triggers_retry_and_fails() {
    // Step 2: prose content is no longer silently accepted as a single work node.
    // The runner validates the accepted content as PlannerOutput and retries.
    // After MAX_PROTOCOL_RETRIES the plan node returns Failed.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Just do the work however you see fit."}"#,
        r#"{"status":"accepted","content":"Still prose, not JSON."}"#,
        r#"{"status":"accepted","content":"Also prose."}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(plan_request("plan the work"), &telemetry);

    let NodeRunResult::Failed(_) = result else {
        panic!("expected Failed after prose planner content exhausts retries");
    };

    // PlannerOutputFallback must NOT be emitted: validation fails in the runner before
    // map_plan_output is reached.
    let records = telemetry.into_records();
    let has_fallback = records.iter().any(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::PlannerOutputFallback
        )
    });
    assert!(
        !has_fallback,
        "PlannerOutputFallback must not be emitted when runner validation fails first"
    );
}

#[test]
fn structured_planner_output_creates_multiple_work_nodes() {
    let tasks_json = r#"{"tasks":[{"id":"alpha","objective":"do alpha","operation":"modify","targets":["alpha.txt"],"depends_on":[]},{"id":"beta","objective":"do beta","operation":"modify","targets":["beta.txt"],"depends_on":["alpha"]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(plan_request("plan the work"), &telemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted");
    };
    assert_eq!(plan.children.len(), 2, "must produce two work nodes");
    assert_eq!(plan.children[0].id, NodeId("alpha".to_string()));
    assert_eq!(plan.children[0].objective, "do alpha");
    assert_eq!(plan.children[0].target_files, vec!["alpha.txt".to_string()]);
    assert!(plan.children[0].dependencies.is_empty());
    assert_eq!(plan.children[1].id, NodeId("beta".to_string()));
    assert_eq!(plan.children[1].objective, "do beta");
    assert_eq!(plan.children[1].target_files, vec!["beta.txt".to_string()]);
    assert_eq!(
        plan.children[1].dependencies,
        vec![NodeId("alpha".to_string())]
    );

    let records = telemetry.into_records();
    let parsed = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::PlannerOutputParsed { .. }
        )
    });
    assert!(parsed.is_some(), "must emit PlannerOutputParsed telemetry");
    if let Some(r) = parsed {
        match &r.event {
            crate::telemetry::TelemetryEvent::PlannerOutputParsed {
                task_count,
                dependency_count,
            } => {
                assert_eq!(*task_count, 2);
                assert_eq!(*dependency_count, 1);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn invalid_structured_plan_returns_failed() {
    // Parses as PlannerOutput but has a self-dependency — validation must fail loudly.
    // All three producer attempts return the same invalid plan, exhausting retries.
    let tasks_json = r#"{"tasks":[{"id":"x","objective":"do x","operation":"modify","targets":["x.txt"],"depends_on":["x"]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json, // Producer attempt 1
        tasks_json, // Producer attempt 2 (retry)
        tasks_json, // Producer attempt 3 (retry)
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(plan_request("plan the work"), &telemetry);

    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed for invalid structured plan");
    };
    assert!(
        failure.message.contains("self-dependency"),
        "failure reason must describe the validation error; got: {}",
        failure.message
    );
    assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));

    // Step 2: validation failure is now recorded as ParseFailed in the runner layer.
    let records = telemetry.into_records();
    let has_parse_failed = records.iter().any(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::ParseFailed { .. }
        )
    });
    assert!(
        has_parse_failed,
        "must emit ParseFailed telemetry for planner validation failure"
    );
}

// --- role policy threading tests ---

#[test]
fn runtime_uses_project_adapter_role_policy() {
    use crate::project::DefaultProjectAdapter;
    use crate::project::ProjectAdapter;

    // Simulate the runtime: get policy from adapter, wire into runner.
    let adapter = DefaultProjectAdapter;
    let policy = adapter.role_policy();

    // A custom marker in a policy derived from the adapter should reach the prompt.
    let custom_policy = crate::roles::RolePolicy {
        worker_producer_system: "ADAPTER_MARKER_TEST".to_string(),
        ..policy
    };

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"completed"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_role_policy(custom_policy);
    runner.run_node(work_request("test policy wiring"), &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    assert!(
        prompts[0].contains("ADAPTER_MARKER_TEST"),
        "adapter role policy must reach the provider prompt; got:\n{}",
        prompts[0]
    );
}

// --- model-tier routing tests ---

#[test]
fn cheap_tier_uses_cheap_provider() {
    // Strong has no responses; calling it would panic. Proves routing is correct.
    let temp = TempDir::new("cheap-tier");
    let cheap = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"task completed\n"}"#,
        r#"{"status":"accepted","content":"task completed"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let strong = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&cheap, &strong);
    let result = runner.run_node(
        work_request_with_artifact("cheap tier test", &temp),
        &NoopTelemetry,
    );
    assert!(
        matches!(result, NodeRunResult::WorkAccepted(_)),
        "cheap tier must route to cheap provider and succeed"
    );
}

#[test]
fn strong_tier_uses_strong_provider() {
    // Cheap has no responses; calling it would panic. Proves routing is correct.
    let temp = TempDir::new("strong-tier");
    let cheap = ScriptedProvider::from_strs(&[]);
    let strong = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"task completed\n"}"#,
        r#"{"status":"accepted","content":"task completed"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&cheap, &strong);
    let result = runner.run_node(
        strong_work_request_with_artifact("strong tier test", &temp),
        &NoopTelemetry,
    );
    assert!(
        matches!(result, NodeRunResult::WorkAccepted(_)),
        "strong tier must route to strong provider and succeed"
    );
}

#[test]
fn strong_tier_uses_strong_token_budget() {
    // Cheap has no responses — if it were called the test would panic.
    let cheap = CapturingProvider::from_strs(&[]);
    let strong = CapturingProvider::from_strs(&[
        r#"{"status":"accepted","content":"task completed"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&cheap, &strong)
        .with_cheap_max_tokens(512)
        .with_strong_max_tokens(2048);
    runner.run_node(strong_work_request("token budget test"), &NoopTelemetry);

    assert_eq!(
        strong.captured_max_tokens(),
        Some(2048),
        "strong tier must use strong_max_tokens"
    );
    assert_eq!(
        cheap.captured_max_tokens(),
        None,
        "cheap provider must not be called for a strong-tier request"
    );
}

// ── read-file enforcement regression test ─────────────────────────────────

#[test]
fn referee_reads_file_and_rejects_default_content_causes_node_failure() {
    // Regression for: Referee accepted even though main.py still contained the
    // default initialized program instead of the required haiku.
    //
    // The Referee must call read_file and inspect file contents before deciding.
    // When the file contents do not satisfy the objective the Referee must reject.
    // Two rounds of rejection (max_revisions = 1) exhaust the revision budget
    // and the node must fail — WorkAccepted must never be returned.
    let temp = TempDir::new("referee-default-content");
    let view = make_artifact_view(&temp, "main.py", r#"print("Hello from forge-lang-init!")"#);

    // Round 1: Producer claims done, Critic reads and accepts, Referee reads
    // main.py, sees default content, and rejects.
    // Round 2: same sequence; budget is now exhausted → node fails.
    let provider = ScriptedProvider::from_strs(&[
        // Round 1
        r#"{"status":"accepted","content":"I wrote the haiku"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"file is present"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
        // Round 2
        r#"{"status":"accepted","content":"I wrote the haiku"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"file is present"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "Write a haiku about Python state machines in main.py".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "node must fail when Referee rejects incorrect file contents"
    );
}

#[test]
fn reviewer_can_read_staged_target_file_with_relative_path() {
    let temp = TempDir::new("reviewer-staged-target");
    let view = make_artifact_view(&temp, "main.py", "print('old')\n");

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"main.py","content":"print('new')\n"}"#,
        r#"{"status":"accepted","content":"updated main.py"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"main.py contains the staged update"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"approved staged main.py"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "Update the program.".to_string(),
        target_files: vec!["main.py".to_string()],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };

    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::WorkAccepted(work) = result else {
        panic!("expected WorkAccepted");
    };
    let update = work
        .artifact_update
        .expect("producer write_file must produce artifact update");
    assert!(
        matches!(&update.changes[0], FileChange::Write { path, .. } if path == "main.py"),
        "staged target write must be preserved; got {:?}",
        update.changes
    );
}

#[test]
fn producer_read_file_does_not_satisfy_critic_read_requirement() {
    // The read_file_executed flag is scoped per role invocation.
    // Even if the Producer successfully read a file, the Critic's own
    // invocation starts fresh with read_file_executed = false.
    // A Critic that never reads must fail the enforcement regardless of
    // what the Producer did.
    let temp = TempDir::new("producer-read-no-critic");
    let view = make_artifact_view(&temp, "hello.txt", "hello world\n");

    // Producer: reads hello.txt (success), then accepts.
    // Critic: accepts three times without reading, exhausting protocol retries.
    // The deliberation must fail — not succeed because Producer already read.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"I read the file and it looks correct"}"#,
        // Critic attempt 1: enforcement fires, must-read retry issued
        r#"{"status":"accepted","content":"looks good to me here ok"}"#,
        // Critic attempt 2: enforcement fires again
        r#"{"status":"accepted","content":"still looks good to me now"}"#,
        // Critic attempt 3: enforcement fires, protocol_attempt > MAX_PROTOCOL_RETRIES → fail
        r#"{"status":"accepted","content":"I accept this work done now"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "write the work".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "node must fail when Critic never reads, even though Producer did"
    );
}

// ── no-recreate validation recovery regression ────────────────────────────

#[test]
fn planner_no_recreate_violation_sends_revision_feedback_and_retries() {
    // Regression: planner first outputs a task for '.gitignore' (existing project
    // file not mentioned in the objective). Validation rejects it and sends structured
    // feedback. Planner revises to only include the main.py task. Run continues to
    // PlanAccepted — the run must NOT terminate with a terminal failure.
    let temp = TempDir::new("no-recreate-retry");
    let view = make_artifact_view(&temp, ".gitignore", "*.pyc\n__pycache__/\n");

    // First planner response: includes .gitignore task (violates no-recreate).
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Create .gitignore file for the project.","operation":"modify","targets":[".gitignore"],"depends_on":[]},{"id":"task-2","objective":"Write main.py with the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    // Second planner response (after revision feedback): only the main.py task.
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Write main.py with the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails no-recreate, handler retries
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called with the scripted responses.
        objective: "Write a haiku about Python state machines.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner revision");
    };
    assert_eq!(
        plan.children.len(),
        1,
        "revised plan must contain only the main.py task"
    );
    assert_eq!(plan.children[0].objective, "Write main.py with the haiku.");
    assert_eq!(plan.children[0].target_files, vec!["main.py".to_string()]);
}

#[test]
fn planner_missing_test_target_sends_revision_feedback_and_retries() {
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"modify","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails test-target validation
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called with the scripted responses.
        objective: "Print a short haiku about state machines.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner adds test target");
    };
    assert_eq!(plan.children.len(), 2);
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["test_main.py".to_string()]),
        "revised plan must include a test_main.py target"
    );
}

#[test]
fn planner_explicit_target_violation_sends_revision_feedback_and_retries() {
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Modify project configuration.","operation":"modify","targets":["pyproject.toml"],"depends_on":[]},{"id":"task-3","objective":"Add tests for the main.py haiku behavior.","operation":"create","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"create","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;

    let provider = RecordingProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails explicit-target validation
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Two explicit files → fast path does not apply (needs exactly one source file);
        // ExplicitTargetViolation still fires when the planner targets pyproject.toml.
        objective: "Modify main.py and utils.py to print a short haiku.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner removes pyproject.toml");
    };
    assert_eq!(plan.children.len(), 2);
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["main.py".to_string()]),
        "revised plan must include main.py"
    );
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["test_main.py".to_string()]),
        "revised plan must include test_main.py"
    );
    assert!(
        plan.children
            .iter()
            .all(|child| !child.objective.contains("pyproject.toml")),
        "revised plan must reject pyproject.toml"
    );

    let prompts = provider.recorded_prompts();
    assert!(
        prompts.iter().any(|prompt| prompt.contains(
            "The objective explicitly targets main.py, utils.py. \
                 Remove all non-test targets except main.py, utils.py."
        )),
        "retry prompt must contain exact explicit-target feedback; got: {prompts:#?}"
    );
}

#[test]
fn planner_no_recreate_violation_exhausts_retries_returns_failed() {
    // When the planner keeps including tasks for existing files after MAX retries,
    // the run must fail — not silently accept the bad plan.
    let temp = TempDir::new("no-recreate-exhausted");
    let view = make_artifact_view(&temp, ".gitignore", "*.pyc\n");

    // All three producer responses include the .gitignore task.
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Create .gitignore file.","operation":"modify","targets":[".gitignore"],"depends_on":[]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan, // attempt 1
        bad_plan, // attempt 2 (retry 1)
        bad_plan, // attempt 3 (retry 2 — MAX_NO_RECREATE_RETRIES)
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called until retries are exhausted.
        objective: "Write a haiku about Python state machines.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "plan must fail when no-recreate retries are exhausted"
    );
    if let NodeRunResult::Failed(failure) = result {
        assert!(
            failure.message.contains(".gitignore") || failure.message.contains("no-recreate"),
            "failure reason must mention the offending file or constraint; got: {}",
            failure.message
        );
    }
}

// ── fast plan integration tests ──────────────────────────────────────────────

#[test]
fn fast_plan_bypasses_provider_and_emits_telemetry() {
    // When the objective names exactly one source file the fast path must
    // produce PlanAccepted without calling the provider at all.
    // ScriptedProvider panics when exhausted — no responses means any call is a bug.
    let provider = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: "Create a simple Python program in main.py that prints a greeting."
                .to_string(),
            target_files: vec![],
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        },
        &telemetry,
    );

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted from fast path; got another variant");
    };
    assert_eq!(plan.children.len(), 1, "no tests required → one work task");
    assert!(
        plan.children[0].target_files == vec!["main.py".to_string()],
        "fast plan work task must target main.py"
    );

    let records = telemetry.into_records();
    let fast_plan_event = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FastPlanUsed { .. }
        )
    });
    assert!(
        fast_plan_event.is_some(),
        "fast path must emit FastPlanUsed telemetry"
    );
    if let Some(r) = fast_plan_event {
        match &r.event {
            crate::telemetry::TelemetryEvent::FastPlanUsed { task_count } => {
                assert_eq!(*task_count, 1);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn fast_plan_with_tests_required_adds_test_task_and_emits_telemetry() {
    let provider = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: "Create a simple Python program in main.py that prints a greeting."
                .to_string(),
            target_files: vec![],
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        },
        &telemetry,
    );

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted from fast path");
    };
    assert_eq!(plan.children.len(), 2, "tests required → two work tasks");
    assert!(
        plan.children
            .iter()
            .any(|c| c.target_files == vec!["main.py".to_string()]),
        "must have a main.py work task"
    );
    assert!(
        plan.children
            .iter()
            .any(|c| c.target_files == vec!["test_main.py".to_string()]),
        "must have a test_main.py task"
    );

    let records = telemetry.into_records();
    let fast_plan_event = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FastPlanUsed { .. }
        )
    });
    assert!(
        fast_plan_event.is_some(),
        "fast path with tests must emit FastPlanUsed telemetry"
    );
    if let Some(r) = fast_plan_event {
        match &r.event {
            crate::telemetry::TelemetryEvent::FastPlanUsed { task_count } => {
                assert_eq!(*task_count, 2);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn multi_file_objective_falls_through_to_llm_planner() {
    // Two explicit source files → fast path returns None → LLM planner called.
    // Targets must be within the allowed set (main.py, utils.py) to pass validation.
    let tasks_json = r#"{"tasks":[{"id":"w","objective":"add logging","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: "Modify main.py and utils.py to add logging.".to_string(),
            target_files: vec![],
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
        },
        &NoopTelemetry,
    );
    assert!(
        matches!(result, NodeRunResult::PlanAccepted(_)),
        "multi-file objective must fall through to the LLM planner and succeed"
    );
}
