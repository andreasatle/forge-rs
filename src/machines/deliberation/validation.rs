//! Semantic validation helpers for deliberation producers.

use crate::artifacts::ArtifactUpdate;
use crate::node_runner::planner::{
    PlannerOutput, PlannerValidationError, validate_planner_explicit_targets,
    validate_planner_no_recreate, validate_planner_output, validate_planner_tests_required,
};

/// Structured context used to validate planner output for a Plan node.
#[derive(Clone)]
pub(super) struct PlanValidationContext {
    pub(super) top_objective: String,
    pub(super) existing_files: Vec<String>,
    pub(super) requires_tests: bool,
}

pub(super) fn validate_plan_output_for_context(
    planner_out: &PlannerOutput,
    context: &PlanValidationContext,
) -> Result<(), PlannerValidationError> {
    validate_planner_output(planner_out)?;
    validate_planner_explicit_targets(planner_out, &context.top_objective)?;
    validate_planner_no_recreate(planner_out, &context.top_objective, &context.existing_files)?;
    if context.requires_tests {
        validate_planner_tests_required(planner_out)?;
    }
    Ok(())
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
        PlannerValidationError::ExplicitTargetViolation {
            allowed_targets, ..
        } => {
            format!(
                "The objective explicitly targets {}. Remove all non-test targets except {}.",
                allowed_targets.join(", "),
                allowed_targets.join(", ")
            )
        }
        PlannerValidationError::MissingTestsForCodeChange => {
            format!(
                "{error}. Project validation includes a test command, so code changes must include \
                 at least one test-related task and target such as a test file."
            )
        }
        PlannerValidationError::TaskRecreatesExistingFile { .. } => {
            format!(
                "{error}. Remove tasks for existing project files not mentioned in the objective. \
                 Only include tasks for files explicitly named in the run objective."
            )
        }
    }
}

pub(super) fn planner_parse_failure_feedback() -> String {
    "Planner output must be valid PlannerOutput JSON with a top-level tasks array. \
     Return only the structured plan JSON, not prose or markdown."
        .to_string()
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum WorkSemanticValidationError {
    MissingArtifactUpdate,
    EmptyArtifactUpdate,
}

impl std::fmt::Display for WorkSemanticValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkSemanticValidationError::MissingArtifactUpdate => {
                write!(f, "accepted work did not produce an artifact update")
            }
            WorkSemanticValidationError::EmptyArtifactUpdate => {
                write!(f, "accepted work produced an empty artifact update")
            }
        }
    }
}

pub(super) fn validate_work_output(
    artifact_update: Option<&ArtifactUpdate>,
) -> Result<(), WorkSemanticValidationError> {
    match artifact_update {
        None => Err(WorkSemanticValidationError::MissingArtifactUpdate),
        Some(update) if update.changes.is_empty() => {
            Err(WorkSemanticValidationError::EmptyArtifactUpdate)
        }
        Some(_) => Ok(()),
    }
}

pub(super) fn work_validation_feedback(error: &WorkSemanticValidationError) -> String {
    match error {
        WorkSemanticValidationError::MissingArtifactUpdate => {
            "Accepted Work results must modify the artifact. Use a file tool such as write_file, replace_text, or delete_file before returning accepted output.".to_string()
        }
        WorkSemanticValidationError::EmptyArtifactUpdate => {
            "Accepted Work results must include at least one file change. Produce a concrete artifact update before returning accepted output.".to_string()
        }
    }
}
