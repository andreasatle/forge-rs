//! Planner output parsing, validation, and NodeRequest mapping.
//!
//! The planner produces a structured task graph as JSON. This module owns the
//! typed schema, validation rules, and the conversion to scheduler
//! [`NodeRequest`]s.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::machines::scheduler::{NodeId, NodeKind, NodeRequest, PlanOutput, PlannerTaskOutput};

/// A single task in a structured planner response.
#[derive(Deserialize, Serialize, Debug)]
pub struct PlannerTask {
    /// Planner-assigned identifier, unique within the output.
    pub id: String,
    /// Natural-language description of what this task should accomplish.
    pub objective: String,
    /// Bare symbol or concept identifier for this task (e.g. `fibonacci`) —
    /// not a file path or location.
    ///
    /// Required (and validated non-blank) for [`PlannerOutputKind::Task`] and
    /// [`PlannerOutputKind::Plan`] output, whose grammar and protocol footer
    /// ask the planner for it in both cases: a `kind: "plan"` batch may
    /// collapse into a terminal task row just like `kind: "task"` (see
    /// `PlannerOutputProcessor::into_plan`), so any task in such a batch
    /// may need a name. `#[serde(default)]` stays in place because `work`
    /// task schemas carry no `name` field at all — `Work` tasks never become
    /// a terminal task row — so their JSON never includes it.
    #[serde(default)]
    pub name: String,
    /// Worker role this task is assigned to, chosen by the planner from the
    /// adapter's configured worker roles.
    ///
    /// `None` when the planner does not assign a role, in which case the
    /// resulting `NodeRequest` gets no worker role either.
    #[serde(default)]
    pub role: Option<String>,
    /// Explicit artifact files this task is allowed and expected to touch.
    ///
    /// Omitted (defaults empty) by `kind: "plan"` tasks, which escalate to
    /// further planning nodes and carry no file targets yet.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Ids of other tasks in the same output that must complete before this one.
    pub depends_on: Vec<String>,
    /// Canonical symbol/function name this task implements (e.g.
    /// `fibonacci`), distinct from `name`: `name` identifies the task for
    /// dependency references, `function_name` is what downstream teams
    /// should actually call the thing they write.
    ///
    /// Required (and validated non-blank) for [`PlannerOutputKind::Task`]
    /// and [`PlannerOutputKind::Plan`] output, same as `name` — see `name`'s
    /// doc for why `Plan` needs it too. `#[serde(default)]` stays in place
    /// because `work` task schemas carry no `function_name` field at all.
    #[serde(default)]
    pub function_name: String,
    /// Source file path this task concerns (e.g. `main.py`) — the single
    /// authoritative location the planner decided on for the code being
    /// written. Downstream roles derive their own targets (e.g. test files)
    /// from this instead of each independently deciding a path.
    ///
    /// Required (and validated non-blank) for [`PlannerOutputKind::Task`]
    /// and [`PlannerOutputKind::Plan`] output, same as `name`.
    #[serde(default)]
    pub file_path: String,
}

/// Whether a [`NodeKind::Plan`] parent's output's tasks become `Work`
/// children, escalate to further `Plan` nodes, or are pure planner intent
/// with no corresponding scheduler node.
///
/// All tasks in a single [`PlannerOutput`] share one kind — a planner cannot
/// mix concrete work with further sub-planning in the same batch. Absent from
/// the JSON, this defaults to `Work` for backward compatibility with planners
/// that predate recursive planning.
#[derive(Clone, Copy, Deserialize, Serialize, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlannerOutputKind {
    /// Tasks become `Work` children that perform concrete, bounded work.
    #[default]
    Work,
    /// The objective is too complex for direct work: `tasks` become further
    /// `Plan` children instead.
    Plan,
    /// Tasks are pure planner intent — id, objective, and ordering only, with
    /// no file targets, role, or operation. Does not correspond to any
    /// scheduler [`NodeKind`], so `PlannerOutputProcessor::into_plan`
    /// produces no children for it, carrying the tasks in
    /// [`crate::machines::scheduler::PlanOutput::tasks`] instead. The
    /// scheduler records these into `.forge/tasks.json` via
    /// `SchedulerEffect::IntegratePlannerTasks`. Triggering worker teams from
    /// the manifest is not yet implemented.
    Task,
}

/// The structured JSON output a [`NodeKind::Plan`] parent's planner is
/// expected to produce.
///
/// Each task becomes a scheduler [`NodeRequest`]. The `depends_on` entries
/// reference other tasks by id within the same output batch.
#[derive(Deserialize, Serialize, Debug)]
pub struct PlannerOutput {
    /// Whether `tasks` become `Work` or `Plan` children. Defaults to `Work`.
    #[serde(default)]
    pub kind: PlannerOutputKind,
    /// The ordered list of tasks the planner wants the scheduler to execute.
    ///
    /// Always required in the JSON (never defaulted): a mandatory `tasks`
    /// field is what lets parsing reject unrelated JSON shapes (e.g. the
    /// Critic/Referee accept-or-reject wrapper) instead of silently matching
    /// as an empty `PlannerOutput`.
    pub tasks: Vec<PlannerTask>,
}

/// Reasons a structured planner output fails validation.
#[derive(Debug, PartialEq)]
pub enum PlannerValidationError {
    /// The plan contains no tasks.
    EmptyTaskList,
    /// Two tasks share the same id.
    DuplicateId(String),
    /// A task has an empty (or whitespace-only) objective.
    EmptyObjective(String),
    /// A `kind: "task"` or `kind: "plan"` task has an empty (or
    /// whitespace-only) name. `kind: "work"` tasks are exempt: they never
    /// become a terminal task row, so they carry no `name` at all.
    EmptyName(String),
    /// A `kind: "task"` or `kind: "plan"` task has an empty (or
    /// whitespace-only) `function_name`. Exempt for the same reason as
    /// `EmptyName`.
    EmptyFunctionName(String),
    /// A `kind: "task"` or `kind: "plan"` task has an empty (or
    /// whitespace-only) `file_path`. Exempt for the same reason as
    /// `EmptyName`.
    EmptyFilePath(String),
    /// A work task does not declare any concrete target files.
    EmptyTargets(String),
    /// A task lists its own id in `depends_on`.
    SelfDependency(String),
    /// A task's `depends_on` references an id not present in the output.
    UnknownDependency {
        /// The id of the task containing the invalid reference.
        task_id: String,
        /// The unknown dependency id that was referenced.
        dep_id: String,
    },
    /// The adapter defines worker roles, but a work task was not assigned a
    /// role matching one of them.
    MissingTaskRole {
        /// The id of the task missing a valid role assignment.
        task_id: String,
    },
}

impl std::fmt::Display for PlannerValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlannerValidationError::EmptyTaskList => {
                write!(
                    f,
                    "The objective requires work, but the submitted plan contains no tasks. \
                     Create one or more bounded tasks that satisfy the objective."
                )
            }
            PlannerValidationError::DuplicateId(id) => {
                write!(f, "duplicate task id: {id}")
            }
            PlannerValidationError::EmptyObjective(id) => {
                write!(f, "empty objective for task: {id}")
            }
            PlannerValidationError::EmptyName(id) => {
                write!(f, "empty name for task: {id}")
            }
            PlannerValidationError::EmptyFunctionName(id) => {
                write!(f, "empty function_name for task: {id}")
            }
            PlannerValidationError::EmptyFilePath(id) => {
                write!(f, "empty file_path for task: {id}")
            }
            PlannerValidationError::EmptyTargets(id) => {
                write!(f, "empty targets for task: {id}")
            }
            PlannerValidationError::SelfDependency(id) => {
                write!(f, "self-dependency in task: {id}")
            }
            PlannerValidationError::UnknownDependency { task_id, dep_id } => {
                write!(f, "task {task_id} depends on unknown id: {dep_id}")
            }
            PlannerValidationError::MissingTaskRole { task_id } => {
                write!(f, "task {task_id} was not assigned a valid worker role")
            }
        }
    }
}

pub(crate) struct PlannerOutputProcessor<'a> {
    /// The adapter's configured worker role name/description pairs. Empty
    /// when the adapter defines no worker roles, in which case task role
    /// assignment is not validated.
    available_worker_roles: &'a [(String, String)],
}

impl<'a> PlannerOutputProcessor<'a> {
    pub(crate) fn new(available_worker_roles: &'a [(String, String)]) -> Self {
        Self {
            available_worker_roles,
        }
    }

    /// Attempt to parse raw provider content as a [`PlannerOutput`].
    pub(crate) fn parse_content(&self, content: &str) -> Option<PlannerOutput> {
        serde_json::from_str::<PlannerOutput>(content).ok()
    }

    /// Parse a raw provider response as a [`PlannerOutput`] directly.
    pub(crate) fn parse_response(&self, raw: &str) -> Result<PlannerOutput, String> {
        let text = raw.trim();
        if !text.starts_with('{') {
            return Err(
                "planner response must start with '{'; preamble text is not permitted".to_string(),
            );
        }
        serde_json::from_str::<PlannerOutput>(text)
            .map_err(|e| format!("planner JSON parse error: {e}"))
    }

    /// Validate structural constraints for a [`NodeKind::Plan`] parent's
    /// output.
    pub(crate) fn validate_structure(
        &self,
        output: &PlannerOutput,
    ) -> Result<(), PlannerValidationError> {
        if output.tasks.is_empty() {
            return Err(PlannerValidationError::EmptyTaskList);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for task in &output.tasks {
            if !seen.insert(task.id.as_str()) {
                return Err(PlannerValidationError::DuplicateId(task.id.clone()));
            }
            if task.objective.trim().is_empty() {
                return Err(PlannerValidationError::EmptyObjective(task.id.clone()));
            }
            if output.kind != PlannerOutputKind::Work && task.name.trim().is_empty() {
                return Err(PlannerValidationError::EmptyName(task.id.clone()));
            }
            if output.kind != PlannerOutputKind::Work && task.function_name.trim().is_empty() {
                return Err(PlannerValidationError::EmptyFunctionName(task.id.clone()));
            }
            if output.kind != PlannerOutputKind::Work && task.file_path.trim().is_empty() {
                return Err(PlannerValidationError::EmptyFilePath(task.id.clone()));
            }
            if output.kind == PlannerOutputKind::Work {
                if task.targets.is_empty() || task.targets.iter().any(|t| t.trim().is_empty()) {
                    return Err(PlannerValidationError::EmptyTargets(task.id.clone()));
                }
                if !self.available_worker_roles.is_empty() {
                    let role_is_valid = task.role.as_deref().is_some_and(|role| {
                        self.available_worker_roles
                            .iter()
                            .any(|(name, _)| name == role)
                    });
                    if !role_is_valid {
                        return Err(PlannerValidationError::MissingTaskRole {
                            task_id: task.id.clone(),
                        });
                    }
                }
            }
            if task.depends_on.iter().any(|d| d == &task.id) {
                return Err(PlannerValidationError::SelfDependency(task.id.clone()));
            }
        }
        let all_ids: HashSet<&str> = output.tasks.iter().map(|t| t.id.as_str()).collect();
        for task in &output.tasks {
            for dep in &task.depends_on {
                if !all_ids.contains(dep.as_str()) {
                    return Err(PlannerValidationError::UnknownDependency {
                        task_id: task.id.clone(),
                        dep_id: dep.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate(&self, output: &PlannerOutput) -> Result<(), PlannerValidationError> {
        self.validate_structure(output)
    }

    /// Convert a validated [`PlannerOutput`] into a [`PlanOutput`] of child
    /// [`NodeRequest`]s.
    ///
    /// `Task`-kind output produces no children: it does not correspond to any
    /// scheduler [`NodeKind`]. Its tasks are carried instead in
    /// [`PlanOutput::tasks`], which the scheduler records into
    /// `.forge/tasks.json` via `SchedulerEffect::IntegratePlannerTasks`.
    pub(crate) fn into_plan(
        self,
        output: PlannerOutput,
        team: String,
        adapter: String,
        northstar: String,
    ) -> PlanOutput {
        let child_kind = match output.kind {
            PlannerOutputKind::Work => NodeKind::Work,
            // A "plan" that yields fewer than two tasks never actually
            // decomposed anything, regardless of whether the lone task's
            // objective matches the parent's verbatim or is reworded. Treat
            // it as a terminal Task output instead of recursing through
            // another Plan round.
            PlannerOutputKind::Plan if output.tasks.len() >= 2 => NodeKind::Plan,
            PlannerOutputKind::Plan | PlannerOutputKind::Task => {
                return PlanOutput {
                    children: vec![],
                    tasks: output
                        .tasks
                        .into_iter()
                        .map(|task| PlannerTaskOutput {
                            id: task.id,
                            objective: task.objective,
                            name: task.name,
                            function_name: task.function_name,
                            file_path: task.file_path,
                            depends_on: task.depends_on,
                        })
                        .collect(),
                };
            }
        };

        PlanOutput {
            children: output
                .tasks
                .into_iter()
                .map(|task| NodeRequest {
                    id: NodeId(task.id),
                    kind: child_kind.clone(),
                    team: team.clone(),
                    task_id: None,
                    adapter: adapter.clone(),
                    northstar: northstar.clone(),
                    worker_role: task.role,
                    objective: task.objective,
                    target_files: task.targets,
                    required_validation_targets: vec![],
                    dependencies: task.depends_on.into_iter().map(NodeId).collect(),
                    validation_plan: None,
                })
                .collect(),
            tasks: vec![],
        }
    }
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod tests;
