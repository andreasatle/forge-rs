use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::artifacts::{Artifact, ArtifactView, create_temporary_workspace};
use crate::machines::scheduler::{
    FailureKind, ModelTier, NodeId, NodeKind, RecoveryAction, TestPlanContext,
};
use crate::node_runner::WorkAttempt;
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
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
        work_attempt: None,
    }
}

fn work_request(objective: &str) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
        work_attempt: None,
    }
}

fn strong_work_request(objective: &str) -> NodeRunRequest {
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Strong,
        attempt: 0,
        artifact_view: None,
        work_attempt: None,
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
    let view = make_artifact_view(temp, "hello.txt", "world\n");
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view.clone()),
        work_attempt: Some(work_attempt_for_view(&view)),
    }
}

fn strong_work_request_with_artifact(objective: &str, temp: &TempDir) -> NodeRunRequest {
    let view = make_artifact_view(temp, "hello.txt", "world\n");
    NodeRunRequest {
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Strong,
        attempt: 0,
        artifact_view: Some(view.clone()),
        work_attempt: Some(work_attempt_for_view(&view)),
    }
}

fn work_attempt_for_view(view: &ArtifactView) -> WorkAttempt {
    let artifact = Artifact {
        repo_path: view.repo_path.clone(),
        branch: "main".to_string(),
        commit_sha: view.commit_sha.clone(),
    };
    let workspace = create_temporary_workspace(&artifact)
        .expect("test artifact view must create a temporary WorkAttempt workspace");
    WorkAttempt {
        attempt: 0,
        workspace: Rc::new(RefCell::new(workspace)),
    }
}

mod artifact;
mod context;
mod failure;
mod fast_plan;
mod output;
mod request;
mod validation;
