//! Artifact update staging and integration for scheduler work nodes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::artifacts::{
    Artifact, TaskRecord, Workspace, WorkspaceFactory, integrate, record_planner_tasks, record_task,
};
use crate::machines::scheduler::event::SchedulerEvent;
use crate::machines::scheduler::failure::FailureKind;
use crate::machines::scheduler::graph::NodeId;
use crate::machines::scheduler::types::{
    IntegrationFailure, IntegrationOutput, PlannerTaskOutput, RecoveryAction, WorkOutput,
};
use crate::node_runner::WorkAttempt;
use crate::services::time::utc_now_iso8601;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::validation::{AlwaysPassValidator, ValidationPlan, ValidationResult, Validator};

use super::validation::validation_retry_message;

type AttemptKey = (NodeId, u32);
type AttemptWorkspace = Rc<RefCell<Workspace>>;

pub(crate) struct IntegrationService {
    artifact: RefCell<Option<Artifact>>,
    pending_attempts: RefCell<HashMap<AttemptKey, AttemptWorkspace>>,
    failed_attempts: RefCell<HashMap<AttemptKey, String>>,
    validator: Rc<dyn Validator>,
    last_validation_passed: RefCell<Option<bool>>,
    telemetry: Rc<dyn TelemetrySink>,
}

impl IntegrationService {
    pub(crate) fn without_artifact(telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self {
            artifact: RefCell::new(None),
            pending_attempts: RefCell::new(HashMap::new()),
            failed_attempts: RefCell::new(HashMap::new()),
            validator: Rc::new(AlwaysPassValidator),
            last_validation_passed: RefCell::new(None),
            telemetry,
        }
    }

    pub(crate) fn with_artifact(artifact: Artifact, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self {
            artifact: RefCell::new(Some(artifact)),
            pending_attempts: RefCell::new(HashMap::new()),
            failed_attempts: RefCell::new(HashMap::new()),
            validator: Rc::new(AlwaysPassValidator),
            last_validation_passed: RefCell::new(None),
            telemetry,
        }
    }

    pub(crate) fn with_validator(self, validator: Rc<dyn Validator>) -> Self {
        Self { validator, ..self }
    }

    pub(crate) fn with_telemetry(self, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self { telemetry, ..self }
    }

    pub(crate) fn artifact(&self) -> Option<Artifact> {
        self.artifact.borrow().clone()
    }

    pub(crate) fn validation_passed(&self) -> Option<bool> {
        *self.last_validation_passed.borrow()
    }

    pub(crate) fn prepare_work_attempt(
        &self,
        node_id: NodeId,
        attempt: u32,
    ) -> Option<WorkAttempt> {
        let artifact = self.artifact.borrow().clone()?;
        let workspace = match WorkspaceFactory::new(&artifact).create_temporary_workspace() {
            Ok(workspace) => workspace,
            Err(err) => {
                let message = format!("worktree creation failed: {err}");
                eprintln!(
                    "[integration] failed to create worktree for {} attempt {}: {}",
                    node_id.0, attempt, message
                );
                self.record_work_attempt_failure(&node_id, attempt, message);
                return None;
            }
        };
        let workspace = Rc::new(RefCell::new(workspace));
        self.pending_attempts
            .borrow_mut()
            .insert((node_id, attempt), Rc::clone(&workspace));
        Some(WorkAttempt { attempt, workspace })
    }

    pub(crate) fn discard_work_attempt_with_reason(
        &self,
        node_id: &NodeId,
        attempt: u32,
        reason: String,
    ) {
        let workspace = self
            .pending_attempts
            .borrow_mut()
            .remove(&(node_id.clone(), attempt));
        if let Some(workspace) = workspace {
            self.record_attempt_evidence(node_id, attempt, &workspace.borrow(), reason);
        }
        self.failed_attempts
            .borrow_mut()
            .remove(&(node_id.clone(), attempt));
    }

    pub(crate) fn record_work_attempt_failure(
        &self,
        node_id: &NodeId,
        attempt: u32,
        error: String,
    ) {
        self.failed_attempts
            .borrow_mut()
            .insert((node_id.clone(), attempt), error);
    }

    pub(crate) fn integrate_work(
        &self,
        node_id: NodeId,
        objective: String,
        work: WorkOutput,
        attempt: u32,
        target_files: Vec<String>,
        validation_plan: Option<ValidationPlan>,
    ) -> SchedulerEvent {
        eprintln!("[integration] start {}", node_id.short());

        let failed_attempt = self
            .failed_attempts
            .borrow_mut()
            .remove(&(node_id.clone(), attempt));
        if let Some(error) = failed_attempt {
            self.discard_work_attempt_with_reason(&node_id, attempt, error.clone());
            return SchedulerEvent::IntegrationFailed {
                node_id,
                failure: integration_failure(error),
            };
        }

        let pending_workspace = self
            .pending_attempts
            .borrow_mut()
            .remove(&(node_id.clone(), attempt));
        let artifact_snapshot = self.artifact.borrow().clone();

        if let (Some(workspace), Some(artifact)) = (pending_workspace, artifact_snapshot) {
            let changed_files = changed_paths(&workspace.borrow());
            if changed_files.is_empty() {
                return SchedulerEvent::IntegrationFailed {
                    node_id,
                    failure: IntegrationFailure {
                        kind: FailureKind::WorkSemanticValidationFailure,
                        message: no_diff_work_message(),
                        recovery: RecoveryAction::Retry {
                            message: no_diff_work_message(),
                        },
                    },
                };
            }
            self.telemetry.record(TelemetryRecord::new(
                "Integration",
                TelemetryEvent::ValidationStarted,
            ));
            let result = run_validation(
                &workspace.borrow(),
                validation_plan.as_ref(),
                &*self.validator,
                &target_files,
                &changed_files,
            );
            if result.passed {
                *self.last_validation_passed.borrow_mut() = Some(true);
                self.telemetry.record(TelemetryRecord::new(
                    "Integration",
                    TelemetryEvent::ValidationPassed {
                        summary: result.summary,
                    },
                ));
                match integrate(&artifact, &workspace.borrow()) {
                    Ok(new_artifact) => {
                        let record = TaskRecord {
                            id: node_id.0.clone(),
                            objective: objective.clone(),
                            targets: target_files.clone(),
                            commit: new_artifact.commit_sha.clone(),
                            completed_at: utc_now_iso8601(),
                            team: None,
                        };
                        let recorded = record_task(&new_artifact, &workspace.borrow(), record)
                            .unwrap_or_else(|err| {
                                eprintln!(
                                    "[integration] task manifest update failed for {}: {}",
                                    node_id.short(),
                                    err
                                );
                                new_artifact
                            });
                        *self.artifact.borrow_mut() = Some(recorded);
                    }
                    Err(err) => {
                        let message = err.to_string();
                        self.record_attempt_evidence(
                            &node_id,
                            attempt,
                            &workspace.borrow(),
                            message.clone(),
                        );
                        return SchedulerEvent::IntegrationFailed {
                            node_id,
                            failure: integration_failure(message),
                        };
                    }
                }
            } else {
                *self.last_validation_passed.borrow_mut() = Some(false);
                let diagnostic_message =
                    validation_retry_message(&result.summary, result.failure.as_ref());
                self.telemetry.record(TelemetryRecord::new(
                    "Integration",
                    TelemetryEvent::ValidationFailed {
                        summary: result.summary.clone(),
                        command: result
                            .failure
                            .as_ref()
                            .map(|failure| failure.command.clone()),
                        exit_code: result
                            .failure
                            .as_ref()
                            .and_then(|failure| failure.exit_code),
                        stdout: result
                            .failure
                            .as_ref()
                            .map(|failure| failure.stdout.clone()),
                        stderr: result
                            .failure
                            .as_ref()
                            .map(|failure| failure.stderr.clone()),
                    },
                ));
                self.record_attempt_evidence(
                    &node_id,
                    attempt,
                    &workspace.borrow(),
                    diagnostic_message.clone(),
                );
                return SchedulerEvent::IntegrationFailed {
                    node_id,
                    failure: IntegrationFailure {
                        kind: FailureKind::ValidationFailure,
                        message: diagnostic_message.clone(),
                        recovery: RecoveryAction::Retry {
                            message: diagnostic_message,
                        },
                    },
                };
            }
        }

        SchedulerEvent::IntegrationSucceeded {
            node_id,
            output: IntegrationOutput {
                summary: work.summary,
            },
        }
    }

    /// Writes planner-produced task records into `.forge/tasks.json` as a
    /// standalone commit, with no accompanying code change to amend into.
    ///
    /// Parallel to [`Self::integrate_work`]'s manifest write, but for
    /// `PlannerOutputKind::Task` output, which has no `Work` node or
    /// `WorkAttempt` workspace of its own.
    pub(crate) fn integrate_planner_tasks(&self, records: Vec<TaskRecord>) -> Result<(), String> {
        let Some(artifact) = self.artifact.borrow().clone() else {
            return Ok(());
        };
        let workspace = WorkspaceFactory::new(&artifact)
            .create_temporary_workspace()
            .map_err(|err| format!("worktree creation failed: {err}"))?;
        let recorded =
            record_planner_tasks(&artifact, &workspace, records).map_err(|err| err.to_string())?;
        *self.artifact.borrow_mut() = Some(recorded);
        Ok(())
    }

    /// Converts a completed `Plan` node's `Task`-kind output into
    /// `TaskRecord`s and records them, translating the result into a
    /// `SchedulerEvent`.
    ///
    /// Parallel to [`Self::integrate_work`], but planner-produced task intent
    /// has no per-task commit of its own, so `commit` is left empty. `team`
    /// stays `None` until team identity is threaded through by the
    /// multi-team scheduler.
    pub(crate) fn integrate_plan_tasks(
        &self,
        node_id: NodeId,
        tasks: Vec<PlannerTaskOutput>,
    ) -> SchedulerEvent {
        let completed_at = utc_now_iso8601();
        let records = tasks
            .into_iter()
            .map(|task| TaskRecord {
                id: task.id,
                objective: task.objective,
                targets: vec![],
                commit: String::new(),
                completed_at: completed_at.clone(),
                team: None,
            })
            .collect();
        match self.integrate_planner_tasks(records) {
            Ok(()) => SchedulerEvent::PlannerTasksIntegrated { node_id },
            Err(message) => SchedulerEvent::PlannerTasksIntegrationFailed {
                node_id,
                failure: integration_failure(message),
            },
        }
    }

    fn record_attempt_evidence(
        &self,
        node_id: &NodeId,
        attempt: u32,
        workspace: &Workspace,
        reason: String,
    ) {
        let changed_files = changed_paths(workspace);
        let git_diff = workspace_diff(workspace, &changed_files);
        self.telemetry.record(TelemetryRecord::new(
            "Integration",
            TelemetryEvent::WorkAttemptDiscarded {
                attempt_id: attempt_id(node_id, attempt),
                node_id: node_id.0.clone(),
                attempt,
                base_commit: workspace.base_commit.clone(),
                changed_files,
                git_diff,
                reason,
            },
        ));
    }
}

fn changed_paths(workspace: &Workspace) -> Vec<String> {
    let output = crate::git::command()
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(workspace.path())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn workspace_diff(workspace: &Workspace, changed_files: &[String]) -> String {
    let mut diff = git_diff(workspace);
    for path in changed_files {
        if is_untracked(workspace, path) {
            if !diff.is_empty() && !diff.ends_with('\n') {
                diff.push('\n');
            }
            diff.push_str(&untracked_file_diff(workspace, path));
        }
    }
    diff
}

fn git_diff(workspace: &Workspace) -> String {
    let output = crate::git::command()
        .args(["diff", "--binary", "HEAD", "--"])
        .current_dir(workspace.path())
        .output();
    let Ok(output) = output else {
        return "(failed to collect git diff)".to_string();
    };
    if !output.status.success() {
        return format!(
            "(failed to collect git diff: {})",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn is_untracked(workspace: &Workspace, path: &str) -> bool {
    let output = crate::git::command()
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(path)
        .current_dir(workspace.path())
        .output();
    matches!(output, Ok(output) if !output.status.success())
}

fn untracked_file_diff(workspace: &Workspace, path: &str) -> String {
    let full_path = workspace.path().join(path);
    let output = crate::git::command()
        .args(["diff", "--no-index", "--binary", "--"])
        .arg("/dev/null")
        .arg(&full_path)
        .current_dir(workspace.path())
        .output();
    let Ok(output) = output else {
        return format!("(failed to collect untracked diff for {path})\n");
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.is_empty() && !output.status.success() && !output.stderr.is_empty() {
        return format!(
            "(failed to collect untracked diff for {path}: {})\n",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    stdout.into_owned()
}

fn attempt_id(node_id: &NodeId, attempt: u32) -> String {
    format!("{}:{attempt}", node_id.0)
}

/// Run validation using the node's `ValidationPlan` when present, falling back
/// to the handler-level `Validator` singleton otherwise.
fn run_validation(
    workspace: &crate::artifacts::Workspace,
    plan: Option<&ValidationPlan>,
    fallback: &dyn Validator,
    target_files: &[String],
    changed_files: &[String],
) -> ValidationResult {
    match plan {
        Some(p) => p.execute_scoped(workspace, target_files, changed_files),
        None => fallback.validate(workspace),
    }
}

fn integration_failure(message: String) -> IntegrationFailure {
    IntegrationFailure {
        kind: FailureKind::IntegrationFailure,
        message: message.clone(),
        recovery: RecoveryAction::Terminal { message },
    }
}

fn no_diff_work_message() -> String {
    "accepted artifact Work produced no file changes in its WorkAttempt workspace".to_string()
}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;
