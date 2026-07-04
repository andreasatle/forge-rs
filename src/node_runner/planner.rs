//! Planner output parsing, validation, and NodeRequest mapping.
//!
//! The planner produces a structured task graph as JSON. This module owns the
//! typed schema, validation rules, and the conversion to scheduler
//! [`NodeRequest`]s.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
    /// Explicit artifact files this task is allowed and expected to touch.
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

/// The structured JSON output the planner is expected to produce.
///
/// Each task becomes a scheduler [`NodeRequest`]. The `depends_on` entries
/// reference other tasks by id within the same output batch.
#[derive(Deserialize, Serialize, Debug)]
pub struct PlannerOutput {
    /// The ordered list of tasks the planner wants the scheduler to execute.
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
    /// A task's target list contains an existing project file that is not
    /// mentioned in the top-level run objective, indicating the planner is
    /// trying to recreate an infrastructure file it should leave alone.
    TaskRecreatesExistingFile {
        /// The task id whose targets contain the pre-existing filename.
        task_id: String,
        /// The filename that already exists and is not an objective target.
        filename: String,
    },
    /// The top-level coding objective explicitly named target files, but a
    /// non-test task targets a different file.
    ExplicitTargetViolation {
        /// The filename targeted by the invalid task.
        filename: String,
        /// Non-test targets explicitly named by the objective.
        allowed_targets: Vec<String>,
    },
    /// Test validation is configured, but a code-changing plan has no test target.
    MissingTestsForCodeChange,
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
            PlannerValidationError::TaskRecreatesExistingFile { task_id, filename } => {
                write!(
                    f,
                    "task {task_id} targets existing file '{filename}' \
                     which is not mentioned in the run objective"
                )
            }
            PlannerValidationError::ExplicitTargetViolation {
                filename,
                allowed_targets,
            } => {
                write!(
                    f,
                    "task targets non-test file '{filename}' but the objective explicitly targets {}",
                    allowed_targets.join(", ")
                )
            }
            PlannerValidationError::MissingTestsForCodeChange => {
                write!(
                    f,
                    "planner output changes code but does not include a test-related target"
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ObjectiveTargetSet {
    targets: HashSet<String>,
}

impl ObjectiveTargetSet {
    fn from_objective(top_objective: &str) -> Self {
        Self {
            targets: top_objective
                .split_whitespace()
                .map(Self::normalize_token)
                .filter(|token| {
                    Self::token_contains_file_separator(token)
                        && Self::token_has_file_extension(token)
                })
                .collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    fn contains(&self, target: &str) -> bool {
        self.targets.contains(target)
    }

    fn has_code_like_target(&self) -> bool {
        self.targets
            .iter()
            .any(|target| PlannerOutputProcessor::target_is_code_like(target))
    }

    fn sorted(&self) -> Vec<String> {
        let mut sorted: Vec<String> = self.targets.iter().cloned().collect();
        sorted.sort();
        sorted
    }

    fn into_vec(self) -> Vec<String> {
        self.targets.into_iter().collect()
    }

    fn normalize_token(token: &str) -> String {
        token
            .trim_matches(|c: char| {
                !(c.is_ascii_alphanumeric()
                    || matches!(c, '.' | '/' | '\\' | '_' | '-' | '@' | '+'))
            })
            .trim_start_matches("./")
            .replace('\\', "/")
    }

    fn token_contains_file_separator(token: &str) -> bool {
        token.contains('.') || token.contains('/')
    }

    fn token_has_file_extension(token: &str) -> bool {
        let Some(filename) = token.rsplit('/').next() else {
            return false;
        };
        let Some((stem, extension)) = filename.rsplit_once('.') else {
            return false;
        };
        !stem.is_empty()
            && !extension.is_empty()
            && extension
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    }
}

pub(crate) struct PlannerOutputProcessor<'a> {
    top_objective: String,
    existing_files: Vec<String>,
    required_test_targets_fn: &'a dyn Fn(&[String]) -> Vec<String>,
    explicit_objective_targets: ObjectiveTargetSet,
}

impl<'a> PlannerOutputProcessor<'a> {
    pub(crate) fn new<I, S>(
        top_objective: impl Into<String>,
        existing_files: I,
        required_test_targets_fn: &'a dyn Fn(&[String]) -> Vec<String>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let top_objective = top_objective.into();
        let explicit_objective_targets = ObjectiveTargetSet::from_objective(&top_objective);
        Self {
            top_objective,
            existing_files: existing_files
                .into_iter()
                .map(|file| file.as_ref().to_string())
                .collect(),
            required_test_targets_fn,
            explicit_objective_targets,
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

    /// Validate structural constraints that do not require run context.
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
            if task.targets.is_empty() || task.targets.iter().any(|t| t.trim().is_empty()) {
                return Err(PlannerValidationError::EmptyTargets(task.id.clone()));
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
        let objective_targets = self.explicit_objective_targets.clone().into_vec();
        let exempt_targets = (self.required_test_targets_fn)(&objective_targets);
        self.validate_explicit_targets(output, &exempt_targets)?;
        self.validate_no_recreate(output)?;
        self.validate_tests_required(output)?;
        Ok(())
    }

    pub(crate) fn validate_explicit_targets(
        &self,
        output: &PlannerOutput,
        exempt_targets: &[String],
    ) -> Result<(), PlannerValidationError> {
        if self.explicit_objective_targets.is_empty()
            || !self.explicit_objective_targets.has_code_like_target()
        {
            return Ok(());
        }

        for target in output.tasks.iter().flat_map(|task| task.targets.iter()) {
            let normalized = ObjectiveTargetSet::normalize_token(target);
            if !exempt_targets.contains(&normalized)
                && !self.explicit_objective_targets.contains(&normalized)
            {
                return Err(PlannerValidationError::ExplicitTargetViolation {
                    filename: normalized,
                    allowed_targets: self.explicit_objective_targets.sorted(),
                });
            }
        }

        Ok(())
    }

    pub(crate) fn validate_no_recreate(
        &self,
        output: &PlannerOutput,
    ) -> Result<(), PlannerValidationError> {
        for task in &output.tasks {
            for filename in &self.existing_files {
                if task.targets.iter().any(|target| target == filename)
                    && !self.top_objective.contains(filename)
                {
                    return Err(PlannerValidationError::TaskRecreatesExistingFile {
                        task_id: task.id.clone(),
                        filename: filename.clone(),
                    });
                }
            }
        }
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

    pub(crate) fn into_plan(self, output: PlannerOutput) -> PlanOutput {
        let all_targets: Vec<String> = output
            .tasks
            .iter()
            .flat_map(|task| task.targets.iter().cloned())
            .collect();
        let validation_targets: HashSet<String> = (self.required_test_targets_fn)(&all_targets)
            .into_iter()
            .collect();

        PlanOutput {
            children: output
                .tasks
                .into_iter()
                .map(|task| {
                    let worker_role = if !task.targets.is_empty()
                        && task
                            .targets
                            .iter()
                            .all(|target| validation_targets.contains(target))
                    {
                        Some("tester".to_string())
                    } else {
                        None
                    };
                    NodeRequest {
                        id: NodeId(task.id),
                        kind: NodeKind::Work,
                        worker_role,
                        objective: task.objective,
                        target_files: task.targets,
                        required_validation_targets: vec![],
                        dependencies: task.depends_on.into_iter().map(NodeId).collect(),
                        validation_plan: None,
                    }
                })
                .collect(),
        }
    }

    fn target_is_code_like(target: &str) -> bool {
        let extension = target
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        matches!(
            extension.as_str(),
            "c" | "cc"
                | "cpp"
                | "cs"
                | "go"
                | "java"
                | "js"
                | "jsx"
                | "kt"
                | "m"
                | "mm"
                | "php"
                | "py"
                | "rb"
                | "rs"
                | "scala"
                | "swift"
                | "ts"
                | "tsx"
        )
    }
}

#[cfg(test)]
fn no_required_test_targets(_: &[String]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod tests;
