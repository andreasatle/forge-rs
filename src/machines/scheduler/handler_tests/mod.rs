use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::artifacts::{Artifact, ArtifactView, WorkspaceFileOps};
use crate::engine::{Machine, run_machine};
use crate::machines::scheduler::effect::SchedulerEffect;
use crate::machines::scheduler::event::{
    FailureKind, IntegrationFailure, IntegrationOutcome, NodeFailure, NodeOutcome, RecoveryAction,
    SchedulerEvent, WorkOutput,
};
use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
use crate::machines::scheduler::state::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest,
    SchedulerState, TestPlanContext,
};
use crate::node_runner::runner::NodeRunner;
use crate::node_runner::types::{NodeRunRequest, NodeRunResult};
use crate::node_runner::{NodeRunWorkResult, StaticNodeRunner};
use crate::telemetry::{TelemetryEvent, TelemetrySink, VecTelemetry};

// ── test helpers ──────────────────────────────────────────────────────────

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-handler-test-{label}-{}-{seq}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("failed to create temporary test directory");
        Self(path)
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .expect("failed to execute git in test");
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn git_output(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("failed to execute git in test");
    assert!(out.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(out.stdout)
        .expect("git output was not UTF-8")
        .trim()
        .to_owned()
}

fn git_clone_bare(source: &Path, destination: &Path) {
    let status = Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(source)
        .arg(destination)
        .status()
        .expect("failed to create bare test repository");
    assert!(status.success(), "git clone --bare failed");
}

/// Build a bare-repository-backed artifact with a single initial commit.
fn fixture(label: &str) -> (TempDirectory, Artifact) {
    let temp = TempDirectory::new(label);
    let seed_path = temp.join("seed");
    fs::create_dir(&seed_path).expect("failed to create seed directory");
    git(&seed_path, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed_path, &["config", "user.name", "Handler Test"]);
    git(
        &seed_path,
        &["config", "user.email", "handler-test@example.invalid"],
    );
    fs::write(seed_path.join("artifact.txt"), "version one\n")
        .expect("failed to write fixture file");
    git(&seed_path, &["add", "artifact.txt"]);
    git(&seed_path, &["commit", "--quiet", "-m", "Initial"]);
    let repo_path = temp.join("artifact.git");
    git_clone_bare(&seed_path, &repo_path);
    let commit_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    (
        temp,
        Artifact {
            repo_path,
            branch: "main".to_owned(),
            commit_sha,
        },
    )
}

fn handler() -> SchedulerHandler<StaticNodeRunner> {
    SchedulerHandler::new(StaticNodeRunner)
}

fn work_node(id: &str, objective: &str) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        required_test_targets: vec![],
        dependencies: vec![],
        status: NodeStatus::Pending,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
    }
}

fn work_node_with_deps(id: &str, objective: &str, deps: &[&str]) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![],
        required_test_targets: vec![],
        dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
        status: NodeStatus::Pending,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
    }
}

/// Returns a work node result that writes a single file.
struct FileWritingRunner {
    path: String,
    content: String,
}

impl NodeRunner for FileWritingRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let attempt = request
            .work_attempt
            .expect("artifact Work tests must receive a WorkAttempt workspace");
        attempt
            .workspace
            .borrow_mut()
            .write_file(&self.path, &self.content)
            .expect("test runner must write attempt workspace");
        NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: format!("wrote {}", self.path),
            },
        })
    }
}

/// Advance the branch in a bare repo to a new commit without a separate clone.
fn advance_branch_in_bare(bare_repo: &Path, branch: &str) -> String {
    let new_sha_out = Command::new("git")
        .args([
            "-c",
            "user.name=External Advancer",
            "-c",
            "user.email=advance@example.invalid",
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "External advance",
        ])
        .current_dir(bare_repo)
        .output()
        .expect("git commit-tree failed");
    assert!(new_sha_out.status.success(), "git commit-tree must succeed");
    let new_sha = String::from_utf8(new_sha_out.stdout)
        .expect("commit-tree output must be UTF-8")
        .trim()
        .to_owned();

    let refname = format!("refs/heads/{branch}");
    let status = Command::new("git")
        .args(["update-ref", &refname, &new_sha])
        .current_dir(bare_repo)
        .status()
        .expect("git update-ref failed");
    assert!(status.success(), "git update-ref must succeed");
    new_sha
}

use crate::artifacts::Workspace;
use crate::validation::{ValidationCommandFailure, ValidationResult, Validator};

struct AlwaysFailValidator;

impl Validator for AlwaysFailValidator {
    fn validate(&self, _workspace: &Workspace) -> ValidationResult {
        ValidationResult {
            passed: false,
            summary: "intentional failure".to_string(),
            failure: Some(ValidationCommandFailure {
                command: "validator test command".to_string(),
                exit_code: Some(1),
                stdout: "validator stdout".to_string(),
                stderr: "validator stderr".to_string(),
            }),
        }
    }
}

/// Reads a specific file from the workspace and asserts it exists.
struct FileExistsValidator {
    path: String,
    found: Rc<RefCell<bool>>,
}

impl Validator for FileExistsValidator {
    fn validate(&self, workspace: &Workspace) -> ValidationResult {
        let exists = workspace.path().join(&self.path).exists();
        *self.found.borrow_mut() = exists;
        ValidationResult {
            passed: true,
            summary: format!("checked {}", self.path),
            failure: None,
        }
    }
}

struct PanicOnCallValidator;

impl Validator for PanicOnCallValidator {
    fn validate(&self, _workspace: &Workspace) -> ValidationResult {
        panic!("validator must not be called when there is no pending update");
    }
}

#[derive(Debug)]
struct CapturedRequest {
    objective: String,
    target_files: Vec<String>,
    attempt: u32,
}

struct FixOnValidationRetryRunner {
    requests: Rc<RefCell<Vec<CapturedRequest>>>,
}

impl NodeRunner for FixOnValidationRetryRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let content = if request.attempt == 0 {
            "broken\n"
        } else {
            "ok\n"
        };
        request
            .work_attempt
            .as_ref()
            .expect("validation retry work must receive a workspace")
            .workspace
            .borrow_mut()
            .write_file("main.py", content)
            .expect("test runner must write main.py");
        self.requests.borrow_mut().push(CapturedRequest {
            objective: request.objective,
            target_files: request.target_files,
            attempt: request.attempt,
        });
        NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: "wrote main.py".to_string(),
            },
        })
    }
}

struct MainPyValidator;

impl Validator for MainPyValidator {
    fn validate(&self, workspace: &Workspace) -> ValidationResult {
        let content = fs::read_to_string(workspace.path().join("main.py"))
            .expect("main.py must exist during validation");
        if content == "ok\n" {
            return ValidationResult {
                passed: true,
                summary: "main.py ok".to_string(),
                failure: None,
            };
        }
        ValidationResult {
            passed: false,
            summary: "main.py failed validation".to_string(),
            failure: Some(ValidationCommandFailure {
                command: "custom-validator main.py".to_string(),
                exit_code: Some(7),
                stdout: "checked main.py".to_string(),
                stderr: "main.py:1: invalid syntax".to_string(),
            }),
        }
    }
}

mod checkpoint;
mod dispatch;
mod integration;
mod progress;
mod recovery;
mod validation;
mod validation_plan;
