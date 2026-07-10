//! Task manifest recording for completed Work node integrations.
//!
//! After a Work node's changes are committed and pushed by [`super::integrate`],
//! [`record_task`] appends an observability record to `.forge/tasks.json` in
//! the workspace and amends it into the just-created commit.

use std::fmt;
use std::fs;

use serde::{Deserialize, Serialize};

use super::{Artifact, Workspace};

const MANIFEST_PATH: &str = ".forge/tasks.json";
const GITIGNORE_PATH: &str = ".gitignore";
const FORGE_IGNORE_ENTRY: &str = ".forge/";
const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Errors that can occur while recording a task into the manifest.
///
/// Callers treat this as a non-fatal, best-effort failure: the artifact
/// commit produced by `integrate` already stands on its own.
#[derive(Debug)]
pub(crate) enum TaskManifestError {
    /// A filesystem operation on the workspace failed.
    Io(String),
    /// The existing manifest could not be parsed, or the updated manifest
    /// could not be serialized.
    Json(String),
    /// A Git command exited with a non-zero status.
    Git {
        /// Short description of the Git operation (e.g. `"commit --amend"`).
        operation: String,
        /// Captured stderr output from the failed command.
        stderr: String,
    },
}

impl fmt::Display for TaskManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskManifestError::Io(reason) => write!(f, "io error: {reason}"),
            TaskManifestError::Json(reason) => write!(f, "json error: {reason}"),
            TaskManifestError::Git { operation, stderr } => {
                write!(f, "git {operation} failed: {stderr}")
            }
        }
    }
}

impl std::error::Error for TaskManifestError {}

/// A single completed Work node, recorded for observability.
///
/// `pub` (rather than `pub(crate)` like the rest of this module) because it
/// appears in `SchedulerEvent::IntegrationSucceeded`/`PlannerTasksIntegrated`,
/// which is part of the scheduler's public event vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    /// The node's stable identifier.
    pub id: String,
    /// The node's objective, as passed to the runner.
    pub objective: String,
    /// The node's declared target files.
    pub targets: Vec<String>,
    /// The artifact commit this task's changes were integrated into.
    pub commit: String,
    /// UTC ISO 8601 timestamp of when the task was recorded.
    pub completed_at: String,
    /// The team that completed this task. Set from the completing node's
    /// `team` field; `None` only for nodes with no team (the single-team
    /// path, where no trigger evaluation depends on this row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskManifestFile {
    schema_version: u32,
    tasks: Vec<TaskRecord>,
}

/// Appends `record` to `.forge/tasks.json` in `workspace` and amends it into
/// the commit at the tip of `artifact`.
///
/// `artifact` must be the result of a just-succeeded `integrate()` call: its
/// `commit_sha` is used both as the commit to amend and as the CAS lease
/// value for the re-push. Returns the amended `Artifact` plus the full
/// manifest task list (including `record`) on success, so callers can
/// evaluate team triggers against the current manifest without a second
/// read.
pub(crate) fn record_task(
    artifact: &Artifact,
    workspace: &Workspace,
    record: TaskRecord,
) -> Result<(Artifact, Vec<TaskRecord>), TaskManifestError> {
    ensure_forge_ignored(workspace)?;

    let mut manifest = read_manifest(workspace)?;
    manifest.tasks.push(record);
    write_manifest(workspace, &manifest)?;

    run_git(workspace, &["add", "-f", MANIFEST_PATH, GITIGNORE_PATH])?;
    run_git(
        workspace,
        &[
            "-c",
            "user.name=Forge Artifact Prototype",
            "-c",
            "user.email=forge-artifacts@example.invalid",
            "commit",
            "--amend",
            "--no-edit",
            "--quiet",
        ],
    )?;
    let amended_sha = git_stdout(workspace, &["rev-parse", "HEAD"])?;

    push_amended(artifact, workspace, &amended_sha)?;

    Ok((
        Artifact {
            repo_path: artifact.repo_path.clone(),
            branch: artifact.branch.clone(),
            commit_sha: amended_sha,
        },
        manifest.tasks,
    ))
}

/// Appends `records` to `.forge/tasks.json` in `workspace` (checked out at
/// `artifact`'s current tip) and integrates the manifest update as a new,
/// standalone commit.
///
/// Unlike [`record_task`], there is no accompanying code change to amend
/// into: planner-produced task records (`PlannerOutputKind::Task`) carry no
/// file targets, so this creates a fresh commit via the standard CAS
/// integration path instead.
///
/// Returns the amended `Artifact` plus the full manifest task list
/// (including `records`), so callers can evaluate team triggers against the
/// current manifest without a second read.
pub(crate) fn record_planner_tasks(
    artifact: &Artifact,
    workspace: &Workspace,
    records: Vec<TaskRecord>,
) -> Result<(Artifact, Vec<TaskRecord>), TaskManifestError> {
    ensure_forge_ignored(workspace)?;

    let mut manifest = read_manifest(workspace)?;
    manifest.tasks.extend(records);
    write_manifest(workspace, &manifest)?;

    // `.forge/` is gitignored, so it must be force-added before the plain
    // `git add --all` inside `integrate` runs (which respects .gitignore).
    run_git(workspace, &["add", "-f", MANIFEST_PATH, GITIGNORE_PATH])?;

    let integrated =
        super::integrate(artifact, workspace).map_err(|err| TaskManifestError::Git {
            operation: "integrate".to_owned(),
            stderr: err.to_string(),
        })?;
    Ok((integrated, manifest.tasks))
}

/// Adds `.forge/` to the workspace's `.gitignore` when not already present.
fn ensure_forge_ignored(workspace: &Workspace) -> Result<(), TaskManifestError> {
    let path = workspace.path().join(GITIGNORE_PATH);
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(TaskManifestError::Io(e.to_string())),
    };

    let already_ignored = existing
        .lines()
        .any(|line| matches!(line.trim(), ".forge" | ".forge/" | "/.forge" | "/.forge/"));
    if already_ignored {
        return Ok(());
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(FORGE_IGNORE_ENTRY);
    updated.push('\n');
    fs::write(&path, updated).map_err(|e| TaskManifestError::Io(e.to_string()))
}

fn read_manifest(workspace: &Workspace) -> Result<TaskManifestFile, TaskManifestError> {
    let path = workspace.path().join(MANIFEST_PATH);
    match fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|e| TaskManifestError::Json(e.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TaskManifestFile {
            schema_version: MANIFEST_SCHEMA_VERSION,
            tasks: Vec::new(),
        }),
        Err(e) => Err(TaskManifestError::Io(e.to_string())),
    }
}

fn write_manifest(
    workspace: &Workspace,
    manifest: &TaskManifestFile,
) -> Result<(), TaskManifestError> {
    let path = workspace.path().join(MANIFEST_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| TaskManifestError::Io(e.to_string()))?;
    }
    let mut json = serde_json::to_string_pretty(manifest)
        .map_err(|e| TaskManifestError::Json(e.to_string()))?;
    json.push('\n');
    fs::write(&path, json).map_err(|e| TaskManifestError::Io(e.to_string()))
}

/// Pushes the amended commit using the pre-amend `artifact.commit_sha` as the
/// CAS lease value, since that is the commit currently at the branch tip.
fn push_amended(
    artifact: &Artifact,
    workspace: &Workspace,
    new_commit: &str,
) -> Result<(), TaskManifestError> {
    let branch_ref = format!("{new_commit}:refs/heads/{}", artifact.branch);
    let lease_arg = format!(
        "--force-with-lease=refs/heads/{}:{}",
        artifact.branch, artifact.commit_sha
    );
    let output = crate::git::command()
        .args(["push", "--quiet", &lease_arg])
        .arg(&artifact.repo_path)
        .arg(&branch_ref)
        .current_dir(workspace.path())
        .output()
        .map_err(|e| TaskManifestError::Git {
            operation: "push".to_owned(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(TaskManifestError::Git {
            operation: "push".to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn run_git(workspace: &Workspace, args: &[&str]) -> Result<(), TaskManifestError> {
    let op = args.join(" ");
    let output = crate::git::command()
        .args(args)
        .current_dir(workspace.path())
        .output()
        .map_err(|e| TaskManifestError::Git {
            operation: op.clone(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(TaskManifestError::Git {
            operation: op,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn git_stdout(workspace: &Workspace, args: &[&str]) -> Result<String, TaskManifestError> {
    let op = args.join(" ");
    let output = crate::git::command()
        .args(args)
        .current_dir(workspace.path())
        .output()
        .map_err(|e| TaskManifestError::Git {
            operation: op.clone(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(TaskManifestError::Git {
            operation: op,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|e| TaskManifestError::Json(e.to_string()))
        .map(|s| s.trim().to_owned())
}

#[cfg(test)]
#[path = "task_manifest_tests.rs"]
mod tests;
