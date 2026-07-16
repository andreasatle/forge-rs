use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::*;
use crate::artifacts::file_ops::WorkspaceFileOps;
use crate::artifacts::{WorkspaceFactory, integrate};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-task-manifest-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temp dir");
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
    let seed = temp.join("seed");
    fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    fs::write(seed.join("file.txt"), "v1\n").unwrap();
    git(&seed, &["add", "file.txt"]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.join("artifact.git");
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

fn integrate_a_change(temp: &TempDir, artifact: &Artifact) -> Artifact {
    let mut workspace = WorkspaceFactory::new(artifact).create_workspace(temp.join("workspace"));
    workspace
        .write_file("file.txt", "v2\n")
        .expect("write_file");
    integrate(artifact, &workspace).expect("integrate")
}

fn sample_record(commit: &str) -> TaskRecord {
    TaskRecord {
        id: "node-1".to_string(),
        objective: "update file.txt".to_string(),
        commit: commit.to_string(),
        completed_at: "2026-07-10T00:00:00Z".to_string(),
        team: None,
        name: None,
        function_name: None,
        role_targets: vec![],
        depends_on: vec![],
    }
}

/// Recording a task must amend the manifest into the integrated commit, push
/// the amended commit to the branch tip, and add `.forge/` to `.gitignore`.
#[test]
fn record_task_amends_and_pushes_manifest() {
    let (temp, artifact) = fixture("amend-and-push");
    let integrated = integrate_a_change(&temp, &artifact);

    let workspace = WorkspaceFactory::new(&integrated).create_workspace(temp.join("record-ws"));
    let record = sample_record(&integrated.commit_sha);
    let (result, _tasks) = record_task(&integrated, &workspace, record).expect("record_task");

    assert_ne!(
        result.commit_sha, integrated.commit_sha,
        "amending must produce a new commit"
    );
    assert_eq!(
        git_output(&artifact.repo_path, &["rev-parse", "refs/heads/main"]),
        result.commit_sha,
        "branch tip must advance to the amended commit"
    );

    let manifest_blob = git_output(
        &artifact.repo_path,
        &["show", &format!("{}:.forge/tasks.json", result.commit_sha)],
    );
    let manifest: Value = serde_json::from_str(&manifest_blob).unwrap();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["tasks"][0]["id"], "node-1");
    assert_eq!(manifest["tasks"][0]["objective"], "update file.txt");

    let gitignore_blob = git_output(
        &artifact.repo_path,
        &["show", &format!("{}:.gitignore", result.commit_sha)],
    );
    assert!(gitignore_blob.lines().any(|line| line.trim() == ".forge/"));

    // The original file change is still present in the amended commit.
    let file_blob = git_output(
        &artifact.repo_path,
        &["show", &format!("{}:file.txt", result.commit_sha)],
    );
    assert_eq!(file_blob, "v2");
}

/// A second recorded task must append to, not replace, the existing manifest.
#[test]
fn record_task_appends_to_existing_manifest() {
    let (temp, artifact) = fixture("append-existing");
    let first_integrated = integrate_a_change(&temp, &artifact);

    let first_ws = WorkspaceFactory::new(&first_integrated).create_workspace(temp.join("ws-1"));
    let (after_first, _tasks) = record_task(
        &first_integrated,
        &first_ws,
        sample_record(&first_integrated.commit_sha),
    )
    .expect("first record_task");

    let mut second_ws = WorkspaceFactory::new(&after_first).create_workspace(temp.join("ws-2"));
    second_ws
        .write_file("file.txt", "v3\n")
        .expect("write_file");
    let second_integrated = integrate(&after_first, &second_ws).expect("integrate");

    let third_ws = WorkspaceFactory::new(&second_integrated).create_workspace(temp.join("ws-3"));
    let mut second_record = sample_record(&second_integrated.commit_sha);
    second_record.id = "node-2".to_string();
    let (final_artifact, _tasks) =
        record_task(&second_integrated, &third_ws, second_record).expect("second record_task");

    let manifest_blob = git_output(
        &artifact.repo_path,
        &[
            "show",
            &format!("{}:.forge/tasks.json", final_artifact.commit_sha),
        ],
    );
    let manifest: Value = serde_json::from_str(&manifest_blob).unwrap();
    let tasks = manifest["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["id"], "node-1");
    assert_eq!(tasks[1]["id"], "node-2");
}

/// When `.gitignore` already ignores `.forge/`, recording a task must not
/// duplicate the entry.
#[test]
fn record_task_does_not_duplicate_existing_gitignore_entry() {
    let (temp, artifact) = fixture("gitignore-present");
    let seed = temp.join("seed");
    fs::write(seed.join(".gitignore"), ".forge/\n").unwrap();
    git(&seed, &["add", ".gitignore"]);
    git(&seed, &["commit", "--quiet", "-m", "add gitignore"]);
    git(
        &seed,
        &[
            "push",
            "--quiet",
            artifact.repo_path.to_str().unwrap(),
            "main",
        ],
    );
    let refreshed_sha = git_output(&artifact.repo_path, &["rev-parse", "refs/heads/main"]);
    let artifact = Artifact {
        commit_sha: refreshed_sha,
        ..artifact
    };

    let integrated = integrate_a_change(&temp, &artifact);
    let workspace = WorkspaceFactory::new(&integrated).create_workspace(temp.join("record-ws"));
    let (result, _tasks) = record_task(
        &integrated,
        &workspace,
        sample_record(&integrated.commit_sha),
    )
    .expect("record_task");

    let gitignore_blob = git_output(
        &artifact.repo_path,
        &["show", &format!("{}:.gitignore", result.commit_sha)],
    );
    assert_eq!(
        gitignore_blob
            .lines()
            .filter(|l| l.trim() == ".forge/")
            .count(),
        1,
        "existing .forge/ entry must not be duplicated"
    );
}
