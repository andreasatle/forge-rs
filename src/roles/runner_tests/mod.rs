use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::artifacts::{Artifact, ArtifactView, create_temporary_workspace};
use crate::machines::scheduler::{NodeKind, TestPlanContext};
use crate::providers::types::{ProviderError, ProviderErrorKind, ProviderResponse};
use crate::providers::{ProviderClient, ProviderRequest};

mod completion_pressure;
mod decision_pressure;
mod parser;
mod policy;
mod prompt;
mod prompt_schema;
mod protocol;
mod reviewer;
mod target_view;
mod telemetry;
mod tooling;
mod tooling_protocol;

struct FailingProvider {
    kind: ProviderErrorKind,
    message: String,
}

impl ProviderClient for FailingProvider {
    fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        Err(ProviderError {
            kind: self.kind.clone(),
            message: self.message.clone(),
        })
    }
}

struct ScriptedProvider {
    responses: RefCell<VecDeque<String>>,
    requests: RefCell<Vec<ProviderRequest>>,
}

impl ScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl ProviderClient for ScriptedProvider {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.requests.borrow_mut().push(req);
        let content = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("ScriptedProvider: responses exhausted");
        Ok(ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-runner-tools-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &PathBuf, args: &[&str]) {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git failed");
}

fn git_rev(dir: &PathBuf) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse failed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn make_view(label: &str) -> (TempDir, ArtifactView) {
    make_view_with_entries(label, &[("hello.txt", b"hello world\n".as_slice())])
}

fn make_view_with_entries(label: &str, entries: &[(&str, &[u8])]) -> (TempDir, ArtifactView) {
    let temp = TempDir::new(label);
    let seed = temp.0.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    for (path, content) in entries {
        let full_path = seed.join(path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full_path, content).unwrap();
    }
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.0.join("bare.git");
    Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare failed");
    let sha = git_rev(&bare);
    (
        temp,
        ArtifactView {
            repo_path: bare,
            commit_sha: sha,
        },
    )
}

fn make_view_with_n_files(label: &str, n: usize) -> (TempDir, ArtifactView) {
    let temp = TempDir::new(label);
    let seed = temp.0.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    for i in 0..n {
        std::fs::write(seed.join(format!("file{i}.txt")), format!("content-{i}\n")).unwrap();
    }
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.0.join("bare.git");
    Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare failed");
    let sha = git_rev(&bare);
    (
        temp,
        ArtifactView {
            repo_path: bare,
            commit_sha: sha,
        },
    )
}

fn make_role_request(role: DeliberationRole, objective: &str) -> RoleRequest {
    RoleRequest {
        role,
        objective: objective.to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        target_views: vec![],
        producer_content: None,
        critic_content: None,
        feedback: vec![],
        node_kind: NodeKind::Work,
        tool_context: None,
    }
}

fn producer_request(objective: &str) -> RoleRequest {
    make_role_request(DeliberationRole::Producer, objective)
}

fn critic_request(objective: &str, producer_content: &str) -> RoleRequest {
    RoleRequest {
        producer_content: Some(producer_content.to_string()),
        ..make_role_request(DeliberationRole::Critic, objective)
    }
}

fn referee_request(objective: &str, producer_content: &str, critic_content: &str) -> RoleRequest {
    RoleRequest {
        producer_content: Some(producer_content.to_string()),
        critic_content: Some(critic_content.to_string()),
        ..make_role_request(DeliberationRole::Referee, objective)
    }
}

fn plan_request(objective: &str) -> RoleRequest {
    RoleRequest {
        node_kind: NodeKind::Plan,
        ..producer_request(objective)
    }
}

fn with_tool_context(mut request: RoleRequest, view: ArtifactView) -> RoleRequest {
    let writable_workspace = if matches!(request.role, DeliberationRole::Producer) {
        let artifact = Artifact {
            repo_path: view.repo_path.clone(),
            branch: "main".to_string(),
            commit_sha: view.commit_sha.clone(),
        };
        Some(Rc::new(RefCell::new(
            create_temporary_workspace(&artifact).expect("failed to create test workspace"),
        )))
    } else {
        None
    };
    request.tool_context = Some(RoleToolContext {
        artifact_view: Box::new(view),
        writable_workspace,
    });
    request
}

fn with_dummy_tool_context(request: RoleRequest) -> RoleRequest {
    let (temp, view) = make_view("dummy-context");
    std::mem::forget(temp);
    with_tool_context(request, view)
}

fn with_target_files(mut request: RoleRequest, target_files: &[&str]) -> RoleRequest {
    request.target_files = target_files.iter().map(|path| path.to_string()).collect();
    request
}

// --- tool loop tests ---
