//! Semantic validation helpers for deliberation producers.

use std::sync::Arc;

use crate::node_runner::TestTargetsFn;
use crate::node_runner::planner::{
    PlannerOutput, PlannerValidationError, validate_planner_explicit_targets,
    validate_planner_no_recreate, validate_planner_output, validate_planner_tests_required,
};

/// Structured context used to validate planner output for a Plan node.
#[derive(Clone)]
pub(crate) struct PlanValidationContext {
    pub(crate) top_objective: String,
    pub(crate) existing_files: Vec<String>,
    /// Called with all targets in the plan; returns the test-file paths the
    /// project adapter requires for the source files found in that list.
    /// An empty return means no tests are required for this plan.
    pub(crate) required_test_targets_fn: Arc<TestTargetsFn>,
}

pub(super) fn validate_plan_output_for_context(
    planner_out: &PlannerOutput,
    context: &PlanValidationContext,
) -> Result<(), PlannerValidationError> {
    validate_planner_output(planner_out)?;

    // Compute adapter-provided exemptions for the explicit-target check using
    // the source files named in the top-level objective.
    let objective_targets: Vec<String> = {
        use crate::node_runner::planner::explicit_objective_targets_pub;
        explicit_objective_targets_pub(&context.top_objective)
    };
    let exempt_targets = (context.required_test_targets_fn)(&objective_targets);

    validate_planner_explicit_targets(planner_out, &context.top_objective, &exempt_targets)?;
    validate_planner_no_recreate(planner_out, &context.top_objective, &context.existing_files)?;
    validate_planner_tests_required(planner_out, context.required_test_targets_fn.as_ref())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_validation_feedback_recommends_write_file_after_failed_replacement() {
        let feedback =
            work_validation_feedback(&WorkSemanticValidationError::MissingArtifactMutation);

        assert!(
            feedback.contains("Use write_file by default")
                && feedback.contains("replacing most or all of an existing file"),
            "feedback must make write_file the default for whole-file rewrites; got: {feedback}"
        );
        assert!(
            feedback.contains("Use replace_text only for small, localized edits")
                && feedback.contains("exact old string that occurs once"),
            "feedback must restrict replace_text to exact localized edits; got: {feedback}"
        );
        assert!(
            feedback.contains("whitespace, indentation, or formatting differences"),
            "feedback must mention exact-match whitespace sensitivity; got: {feedback}"
        );
        assert!(
            feedback.contains("switch to write_file instead of retrying another replace_text"),
            "feedback must recommend write_file after failed whole-file replace_text attempts; got: {feedback}"
        );
    }
}
