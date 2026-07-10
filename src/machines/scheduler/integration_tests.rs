use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::telemetry::NoopTelemetry;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("forge-int-svc-{label}-{}-{id}", std::process::id()));
        fs::create_dir(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(path: &Path, args: &[&str]) {
    let status = crate::git::command()
        .args(args)
        .current_dir(path)
        .status()
        .expect("git");
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn git_output(path: &Path, args: &[&str]) -> String {
    let output = crate::git::command()
        .args(args)
        .current_dir(path)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture(label: &str) -> (TempDir, Artifact) {
    let temp = TempDir::new(label);
    let seed = temp.0.join("seed");
    fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    fs::write(seed.join("file.txt"), "v1\n").unwrap();
    git(&seed, &["add", "file.txt"]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.0.join("artifact.git");
    let status = crate::git::command()
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare");
    assert!(status.success());
    let sha = git_output(&bare, &["rev-parse", "HEAD"]);
    let artifact = Artifact {
        repo_path: bare,
        branch: "main".to_owned(),
        commit_sha: sha,
    };
    (temp, artifact)
}

/// A `Task`-kind planner output has no `WorkAttempt` workspace of its own, so
/// `integrate_planner_tasks` must create its own manifest-only commit rather
/// than relying on `integrate_work`'s amend path.
#[test]
fn integrate_planner_tasks_creates_manifest_only_commit() {
    let (_temp, artifact) = fixture("planner-tasks-commit");
    let repo_path = artifact.repo_path.clone();
    let original_sha = artifact.commit_sha.clone();

    let service = IntegrationService::with_artifact(artifact, Rc::new(NoopTelemetry));
    let records = vec![
        TaskRecord {
            id: "t1".to_string(),
            objective: "decompose alpha".to_string(),
            targets: vec![],
            commit: String::new(),
            completed_at: "2026-07-10T00:00:00Z".to_string(),
            team: Some("planner".to_string()),
        },
        TaskRecord {
            id: "t2".to_string(),
            objective: "decompose beta".to_string(),
            targets: vec![],
            commit: String::new(),
            completed_at: "2026-07-10T00:00:01Z".to_string(),
            team: Some("planner".to_string()),
        },
    ];

    service
        .integrate_planner_tasks(records)
        .expect("integrate_planner_tasks");

    let new_sha = git_output(&repo_path, &["rev-parse", "refs/heads/main"]);
    assert_ne!(new_sha, original_sha, "must create a new commit");
    assert_eq!(
        service.artifact().expect("artifact present").commit_sha,
        new_sha,
        "service must track the new commit"
    );

    let manifest_blob = git_output(
        &repo_path,
        &["show", &format!("{new_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest_blob).unwrap();
    assert_eq!(manifest["schema_version"], 1);
    let tasks = manifest["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["id"], "t1");
    assert_eq!(tasks[0]["team"], "planner");
    assert_eq!(tasks[1]["id"], "t2");

    // No code file changed — the commit carries only manifest/.gitignore.
    let file_blob = git_output(&repo_path, &["show", &format!("{new_sha}:file.txt")]);
    assert_eq!(file_blob, "v1");
}

/// `integrate_plan_tasks` is the scheduler-facing entry point: it converts
/// `PlannerTaskOutput`s into `TaskRecord`s (leaving `commit` empty, `team` set
/// to the completing node's team) and translates the manifest write into a
/// `SchedulerEvent`, whose `manifest_tasks` carries the resulting records so
/// team trigger evaluation can act on them without a separate manifest read.
#[test]
fn integrate_plan_tasks_returns_planner_tasks_integrated_event() {
    let (_temp, artifact) = fixture("plan-tasks-event");
    let service = IntegrationService::with_artifact(artifact, Rc::new(NoopTelemetry));

    let event = service.integrate_plan_tasks(
        NodeId("P".to_string()),
        vec![
            PlannerTaskOutput {
                id: "t1".to_string(),
                objective: "decompose alpha".to_string(),
            },
            PlannerTaskOutput {
                id: "t2".to_string(),
                objective: "decompose beta".to_string(),
            },
        ],
        "planner".to_string(),
    );

    let SchedulerEvent::PlannerTasksIntegrated {
        node_id,
        manifest_tasks,
    } = event
    else {
        panic!("expected PlannerTasksIntegrated, got {event:#?}");
    };
    assert_eq!(node_id, NodeId("P".to_string()));
    assert_eq!(manifest_tasks.len(), 2);
    assert_eq!(manifest_tasks[0].id, "t1");
    assert_eq!(manifest_tasks[0].team, Some("planner".to_string()));
    assert_eq!(manifest_tasks[1].id, "t2");
    assert_eq!(manifest_tasks[1].team, Some("planner".to_string()));
}
