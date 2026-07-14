//! Human-readable retry feedback text for producer validation failures.
//!
//! [`super::handler::DeliberationHandler`] validates Producer output against
//! structured planner rules and artifact-mutation rules; this module owns the
//! natural-language guidance shown to the model on the resulting retry.

use crate::node_runner::planner::PlannerValidationError;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum WorkSemanticValidationError {
    MissingArtifactMutation,
}

impl std::fmt::Display for WorkSemanticValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkSemanticValidationError::MissingArtifactMutation => {
                write!(f, "accepted work did not mutate the WorkAttempt workspace")
            }
        }
    }
}

pub(super) fn planner_validation_feedback(error: &PlannerValidationError) -> String {
    match error {
        PlannerValidationError::EmptyTaskList => error.to_string(),
        PlannerValidationError::DuplicateId(id) => {
            format!("{error}. Assign a unique id to every task; '{id}' appears more than once.")
        }
        PlannerValidationError::EmptyObjective(id) => {
            format!(
                "{error}. Every task must have a non-empty objective. \
                 Add a clear objective to task '{id}'."
            )
        }
        PlannerValidationError::EmptyName(id) => {
            format!(
                "{error}. Every `kind: \"task\"` task must have a non-empty `name` — a bare \
                 symbol or concept identifier, not a file path. Add a name to task '{id}'."
            )
        }
        PlannerValidationError::EmptyTargets(id) => {
            format!(
                "{error}. Every task must declare at least one concrete target file. \
                 Add a target to task '{id}'."
            )
        }
        PlannerValidationError::SelfDependency(id) => {
            format!(
                "{error}. A task cannot depend on itself. \
                 Remove '{id}' from its own depends_on list."
            )
        }
        PlannerValidationError::UnknownDependency { task_id, dep_id } => {
            format!(
                "{error}. Task '{task_id}' depends on '{dep_id}', which does not exist in this \
                 plan. Only reference task ids defined in the same plan."
            )
        }
        PlannerValidationError::MissingTestsForCodeChange { required } => {
            format!(
                "{error}. Project validation includes a test command, so code changes must include \
                 a task whose targets contain the exact expected test path: {}.",
                required.join(", ")
            )
        }
        PlannerValidationError::MissingTaskRole { task_id } => {
            format!(
                "{error}. Assign task '{task_id}' a `role` matching one of the available worker \
                 roles listed in the prompt."
            )
        }
    }
}

pub(super) fn planner_parse_failure_feedback() -> String {
    "Planner output must be valid PlannerOutput JSON with a top-level tasks array. \
     Return only the structured plan JSON, not prose or markdown."
        .to_string()
}

pub(super) fn validate_work_output(
    artifact_changed: bool,
) -> Result<(), WorkSemanticValidationError> {
    if artifact_changed {
        return Ok(());
    }
    Err(WorkSemanticValidationError::MissingArtifactMutation)
}

pub(super) fn work_validation_feedback(error: &WorkSemanticValidationError) -> String {
    match error {
        WorkSemanticValidationError::MissingArtifactMutation => {
            "Accepted Work results must modify the artifact. Use write_file by default when creating a file or replacing most or all of an existing file. Use replace_text only for small, localized edits after reading the file and providing an exact old string that occurs once; whitespace, indentation, or formatting differences will cause replace_text to fail. If a replace_text attempt could not be validated for a whole-file rewrite, switch to write_file instead of retrying another replace_text.".to_string()
        }
    }
}
