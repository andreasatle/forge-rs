//! Planner output parsing, validation, and NodeRequest mapping.
//!
//! The planner produces a structured task graph as JSON. This module owns the
//! typed schema, validation rules, and the conversion to scheduler
//! [`NodeRequest`]s.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::machines::scheduler::{NodeId, NodeKind, NodeRequest, PlanOutput};

/// A single task in a structured planner response.
#[derive(Deserialize, Serialize, Debug)]
pub struct PlannerTask {
    /// Planner-assigned identifier, unique within the output.
    pub id: String,
    /// Natural-language description of what this task should accomplish.
    pub objective: String,
    /// Concrete artifact operation this task will perform, when the active
    /// adapter's prompt schema asks for one.
    ///
    /// `None` under adapters whose prompt schema omits the field (e.g.
    /// [`crate::project::DefaultProjectAdapter`]). Not read downstream —
    /// kept as adapter-supplied metadata.
    #[serde(default)]
    pub operation: Option<PlannerOperation>,
    /// Worker role this task is assigned to, chosen by the planner from the
    /// adapter's configured worker roles.
    ///
    /// `None` when the planner does not assign a role, in which case the
    /// resulting `NodeRequest` gets no worker role either.
    #[serde(default)]
    pub role: Option<String>,
    /// Explicit artifact files this task is allowed and expected to touch.
    ///
    /// Omitted (defaults empty) by [`NodeKind::Decomposition`]'s
    /// `decomposition` schema, whose tasks carry no file targets.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Ids of other tasks in the same output that must complete before this one.
    pub depends_on: Vec<String>,
}

/// Concrete artifact operation a planner task will perform.
#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlannerOperation {
    /// Create one or more target files.
    Create,
    /// Modify one or more existing target files.
    Modify,
    /// Delete one or more target files.
    Delete,
}

/// Whether a [`NodeKind::Decomposition`] parent's output still spans multiple
/// concerns or has reached an atomic objective.
#[derive(Clone, Copy, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DecompositionOutputKind {
    /// The objective still spans multiple concerns: `objectives` become
    /// further `Decomposition` children.
    Decomposition,
    /// The objective is atomic and ready for a leaf planner: the response
    /// carries no `objectives`, and the parent's own objective becomes a
    /// single `Plan` child.
    Plan,
}

/// A single objective in a [`NodeKind::Decomposition`] parent's structured
/// response.
///
/// Leaner than [`PlannerTask`]: Decomposition objectives carry no concrete
/// file assignment, so there is no `operation`, `role`, or `targets`.
#[derive(Deserialize, Serialize, Debug)]
pub struct DecompositionObjective {
    /// Planner-assigned identifier, unique within the output.
    pub id: String,
    /// Natural-language description of what this objective should accomplish.
    pub objective: String,
    /// Ids of other objectives in the same output that must complete before
    /// this one.
    pub depends_on: Vec<String>,
}

/// The structured JSON output a [`NodeKind::Decomposition`] parent's planner
/// is expected to produce.
///
/// Each objective becomes a scheduler [`NodeRequest`]. The `depends_on`
/// entries reference other objectives by id within the same output batch.
#[derive(Deserialize, Serialize, Debug)]
pub struct DecompositionOutput {
    /// Whether `objectives` become further `Decomposition` children, or the
    /// objective is atomic.
    ///
    /// Mandatory in the JSON (no default): this is what lets parsing reject
    /// unrelated JSON shapes (e.g. the Critic/Referee accept-or-reject
    /// wrapper) instead of silently matching as an empty `DecompositionOutput`.
    pub kind: DecompositionOutputKind,
    /// The ordered list of objectives the planner wants the scheduler to
    /// execute.
    ///
    /// Required (non-empty) when `kind` is `Decomposition`. Omitted (defaults
    /// empty) when `kind` is `Plan`, whose response carries no objective list
    /// at all — just `{"kind": "plan"}`.
    #[serde(default)]
    pub objectives: Vec<DecompositionObjective>,
}

/// Whether a [`NodeKind::Plan`] parent's output's tasks become `Work`
/// children or escalate to further `Decomposition`.
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
    /// `Decomposition` children instead.
    Decomposition,
}

/// The structured JSON output a [`NodeKind::Plan`] parent's planner is
/// expected to produce.
///
/// Each task becomes a scheduler [`NodeRequest`]. The `depends_on` entries
/// reference other tasks by id within the same output batch.
#[derive(Deserialize, Serialize, Debug)]
pub struct PlannerOutput {
    /// Whether `tasks` become `Work` or `Decomposition` children. Defaults to
    /// `Work`.
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
    /// Test validation is configured, but a code-changing plan has no test target.
    MissingTestsForCodeChange,
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
            PlannerValidationError::EmptyTargets(id) => {
                write!(f, "empty targets for task: {id}")
            }
            PlannerValidationError::SelfDependency(id) => {
                write!(f, "self-dependency in task: {id}")
            }
            PlannerValidationError::UnknownDependency { task_id, dep_id } => {
                write!(f, "task {task_id} depends on unknown id: {dep_id}")
            }
            PlannerValidationError::MissingTestsForCodeChange => {
                write!(
                    f,
                    "planner output changes code but does not include a test-related target"
                )
            }
            PlannerValidationError::MissingTaskRole { task_id } => {
                write!(f, "task {task_id} was not assigned a valid worker role")
            }
        }
    }
}

pub(crate) struct PlannerOutputProcessor<'a> {
    required_test_targets_fn: &'a dyn Fn(&[String]) -> Vec<String>,
    /// The adapter's configured worker role name/description pairs. Empty
    /// when the adapter defines no worker roles, in which case task role
    /// assignment is not validated.
    available_worker_roles: &'a [(String, String)],
}

impl<'a> PlannerOutputProcessor<'a> {
    pub(crate) fn new(
        required_test_targets_fn: &'a dyn Fn(&[String]) -> Vec<String>,
        available_worker_roles: &'a [(String, String)],
    ) -> Self {
        Self {
            required_test_targets_fn,
            available_worker_roles,
        }
    }

    /// Attempt to parse raw provider content as a [`DecompositionOutput`].
    pub(crate) fn parse_decomposition_content(&self, content: &str) -> Option<DecompositionOutput> {
        serde_json::from_str::<DecompositionOutput>(content).ok()
    }

    /// Parse a raw provider response as a [`DecompositionOutput`] directly.
    pub(crate) fn parse_decomposition_response(
        &self,
        raw: &str,
    ) -> Result<DecompositionOutput, String> {
        let text = raw.trim();
        if !text.starts_with('{') {
            return Err(
                "planner response must start with '{'; preamble text is not permitted".to_string(),
            );
        }
        serde_json::from_str::<DecompositionOutput>(text)
            .map_err(|e| format!("planner JSON parse error: {e}"))
    }

    /// Validate structural constraints for a [`NodeKind::Decomposition`]
    /// parent's output.
    ///
    /// A `kind: "plan"` response carries no `objectives` at all — the
    /// objective is atomic, so there is nothing to validate; it maps to a
    /// single `Plan` child in [`Self::into_decomposition_plan`]. Decomposition
    /// objectives are never assigned worker roles or concrete targets, so
    /// this never validates either.
    pub(crate) fn validate_decomposition_structure(
        &self,
        output: &DecompositionOutput,
    ) -> Result<(), PlannerValidationError> {
        if output.kind == DecompositionOutputKind::Plan {
            return Ok(());
        }
        if output.objectives.is_empty() {
            return Err(PlannerValidationError::EmptyTaskList);
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for objective in &output.objectives {
            if !seen.insert(objective.id.as_str()) {
                return Err(PlannerValidationError::DuplicateId(objective.id.clone()));
            }
            if objective.objective.trim().is_empty() {
                return Err(PlannerValidationError::EmptyObjective(objective.id.clone()));
            }
            if objective.depends_on.iter().any(|d| d == &objective.id) {
                return Err(PlannerValidationError::SelfDependency(objective.id.clone()));
            }
        }
        let all_ids: HashSet<&str> = output.objectives.iter().map(|o| o.id.as_str()).collect();
        for objective in &output.objectives {
            for dep in &objective.depends_on {
                if !all_ids.contains(dep.as_str()) {
                    return Err(PlannerValidationError::UnknownDependency {
                        task_id: objective.id.clone(),
                        dep_id: dep.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Convert a validated [`DecompositionOutput`] into a [`PlanOutput`] of
    /// child [`NodeRequest`]s.
    ///
    /// `parent_objective` is the objective of the node that produced `output`
    /// — used only when `output.kind` is `Plan`, since that response carries
    /// no task list of its own.
    pub(crate) fn into_decomposition_plan(
        self,
        output: DecompositionOutput,
        parent_objective: &str,
    ) -> PlanOutput {
        if output.kind == DecompositionOutputKind::Plan {
            return PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId(Uuid::new_v4().to_string()),
                    kind: NodeKind::Plan,
                    worker_role: None,
                    objective: parent_objective.to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![],
                    validation_plan: None,
                }],
            };
        }

        PlanOutput {
            children: output
                .objectives
                .into_iter()
                .map(|objective| NodeRequest {
                    id: NodeId(objective.id),
                    kind: NodeKind::Decomposition,
                    worker_role: None,
                    objective: objective.objective,
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: objective.depends_on.into_iter().map(NodeId).collect(),
                    validation_plan: None,
                })
                .collect(),
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
        self.validate_structure(output)?;
        if output.kind == PlannerOutputKind::Decomposition {
            // Escalated tasks have no concrete files yet, so target-based
            // validation does not apply until they are decomposed further.
            return Ok(());
        }
        self.validate_tests_required(output)?;
        Ok(())
    }

    pub(crate) fn validate_tests_required(
        &self,
        output: &PlannerOutput,
    ) -> Result<(), PlannerValidationError> {
        let all_plan_targets: Vec<String> = output
            .tasks
            .iter()
            .flat_map(|task| task.targets.iter().cloned())
            .collect();
        let required = (self.required_test_targets_fn)(&all_plan_targets);
        if required.is_empty() {
            return Ok(());
        }
        let plan_target_set: std::collections::HashSet<&str> =
            all_plan_targets.iter().map(|s| s.as_str()).collect();
        if required
            .iter()
            .any(|r| plan_target_set.contains(r.as_str()))
        {
            Ok(())
        } else {
            Err(PlannerValidationError::MissingTestsForCodeChange)
        }
    }

    /// Convert a validated [`PlannerOutput`] into a [`PlanOutput`] of child
    /// [`NodeRequest`]s.
    pub(crate) fn into_plan(self, output: PlannerOutput) -> PlanOutput {
        let child_kind = match output.kind {
            PlannerOutputKind::Work => NodeKind::Work,
            PlannerOutputKind::Decomposition => NodeKind::Decomposition,
        };

        PlanOutput {
            children: output
                .tasks
                .into_iter()
                .map(|task| NodeRequest {
                    id: NodeId(task.id),
                    kind: child_kind.clone(),
                    worker_role: task.role,
                    objective: task.objective,
                    target_files: task.targets,
                    required_validation_targets: vec![],
                    dependencies: task.depends_on.into_iter().map(NodeId).collect(),
                    validation_plan: None,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
fn no_required_test_targets(_: &[String]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod tests;
