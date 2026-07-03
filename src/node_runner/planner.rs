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

    fn source_code_targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = self
            .targets
            .iter()
            .filter(|target| {
                PlannerOutputProcessor::target_is_code_like(target)
                    && !PlannerOutputProcessor::target_is_test_related(target)
            })
            .cloned()
            .collect();
        targets.sort();
        targets
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
    pub(crate) fn parse_content(content: &str) -> Option<PlannerOutput> {
        serde_json::from_str::<PlannerOutput>(content).ok()
    }

    /// Parse a raw provider response as a [`PlannerOutput`] directly.
    pub(crate) fn parse_response(raw: &str) -> Result<PlannerOutput, String> {
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
    pub(crate) fn validate_structure(output: &PlannerOutput) -> Result<(), PlannerValidationError> {
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
        Self::validate_structure(output)?;
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

    pub(crate) fn try_fast_plan(&self) -> Option<PlanOutput> {
        let source_targets = self.explicit_objective_targets.source_code_targets();

        if source_targets.len() != 1 {
            return None;
        }

        let source = source_targets.into_iter().next().unwrap();
        let required_validation_targets =
            (self.required_test_targets_fn)(std::slice::from_ref(&source));
        let work = NodeRequest {
            id: NodeId("work".to_string()),
            kind: NodeKind::Work,
            objective: self.top_objective.clone(),
            target_files: vec![source.clone()],
            required_validation_targets: required_validation_targets.clone(),
            dependencies: vec![],
            validation_plan: None,
        };
        let mut children = vec![work];

        for (i, test_target) in required_validation_targets.into_iter().enumerate() {
            let id = if i == 0 {
                "tests".to_string()
            } else {
                format!("tests-{i}")
            };
            children.push(NodeRequest {
                id: NodeId(id),
                kind: NodeKind::Work,
                objective: format!(
                    "Write tests that verify the work described by the following objective:\n\n\
                     {}",
                    self.top_objective
                ),
                target_files: vec![test_target],
                required_validation_targets: vec![],
                dependencies: vec![NodeId("work".to_string())],
                validation_plan: None,
            });
        }

        Some(PlanOutput { children })
    }

    pub(crate) fn into_plan_output(output: PlannerOutput) -> PlanOutput {
        PlanOutput {
            children: output
                .tasks
                .into_iter()
                .map(|task| NodeRequest {
                    id: NodeId(task.id),
                    kind: NodeKind::Work,
                    objective: task.objective,
                    target_files: task.targets,
                    required_validation_targets: vec![],
                    dependencies: task.depends_on.into_iter().map(NodeId).collect(),
                    validation_plan: None,
                })
                .collect(),
        }
    }

    fn target_is_test_related(target: &str) -> bool {
        let path = target.replace('\\', "/").to_ascii_lowercase();
        let filename = path.rsplit('/').next().unwrap_or(path.as_str());
        path.contains("/test/")
            || path.contains("/tests/")
            || path.starts_with("test/")
            || path.starts_with("tests/")
            || filename.starts_with("test_")
            || filename.starts_with("test-")
            || filename.ends_with("_test.rs")
            || filename.ends_with("_tests.rs")
            || filename.contains("_test.")
            || filename.contains("-test.")
            || filename.contains(".test.")
            || filename.contains("_tests.")
            || filename.contains("-tests.")
            || filename.contains(".spec.")
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

fn no_required_test_targets(_: &[String]) -> Vec<String> {
    Vec::new()
}

/// Attempt to parse raw provider content as a [`PlannerOutput`].
///
/// Returns `Some(PlannerOutput)` on success, `None` if the content cannot be
/// parsed. A parse failure is not an error in the run — prose output is an
/// expected fallback case that triggers single-work-node behaviour.
pub fn parse_planner_content(content: &str) -> Option<PlannerOutput> {
    PlannerOutputProcessor::parse_content(content)
}

/// Parse a raw provider response as a [`PlannerOutput`] directly.
///
/// Unlike [`parse_planner_content`] this returns a `Result` suitable for the
/// role runner's retry path. A preamble before the opening `{` is rejected
/// immediately without attempting JSON parsing.
pub fn try_parse_planner_response(raw: &str) -> Result<PlannerOutput, String> {
    PlannerOutputProcessor::parse_response(raw)
}

/// Validate the structural constraints of a parsed [`PlannerOutput`].
///
/// Checked invariants:
/// - Task ids are unique within the output.
/// - Every objective is non-empty (after trimming).
/// - Every work task declares at least one non-empty target file.
/// - No task lists itself in `depends_on`.
/// - Every `depends_on` entry names another task in the same output.
///
/// Returns `Err` on the first violation. Does not attempt to repair.
pub fn validate_planner_output(output: &PlannerOutput) -> Result<(), PlannerValidationError> {
    PlannerOutputProcessor::validate_structure(output)
}

/// Check that when a coding objective explicitly names target files, all
/// planner targets are either among those named files or among the
/// `exempt_targets` provided by the project adapter.
///
/// The adapter supplies `exempt_targets` as the required test-file paths for
/// the explicitly-named source files. Test targets are exempt because project
/// validation may require newly-created tests even when the user only names
/// the implementation file. The constraint only applies when the objective
/// names at least one code-like file.
pub fn validate_planner_explicit_targets(
    output: &PlannerOutput,
    top_objective: &str,
    exempt_targets: &[String],
) -> Result<(), PlannerValidationError> {
    let processor = PlannerOutputProcessor::new(
        top_objective,
        std::iter::empty::<&str>(),
        &no_required_test_targets,
    );
    processor.validate_explicit_targets(output, exempt_targets)
}

/// Check that no task in `output` targets an existing project file that is not
/// mentioned in `top_objective`.
///
/// A task is considered to target an existing file when its structured
/// `targets` list contains a filename from `existing_files` AND that filename
/// does not appear in `top_objective`. Objective prose is intentionally ignored
/// because it is not a reliable file-targeting contract.
///
/// Returns `Err` on the first violation found.
pub fn validate_planner_no_recreate(
    output: &PlannerOutput,
    top_objective: &str,
    existing_files: &[impl AsRef<str>],
) -> Result<(), PlannerValidationError> {
    let processor =
        PlannerOutputProcessor::new(top_objective, existing_files, &no_required_test_targets);
    processor.validate_no_recreate(output)
}

/// Check that a code-changing plan includes at least one required test target.
///
/// `required_test_targets_fn` is called with all targets in the plan and
/// returns the set of test-file paths the project adapter requires for those
/// source files. If the adapter requires no tests (returns empty), the check
/// passes unconditionally. Otherwise the plan must contain at least one of
/// the required targets.
///
/// This is intentionally based on structured `targets`, not objective prose.
/// The adapter decides which source files require tests and what their test
/// file names should be; the framework only checks coverage.
pub fn validate_planner_tests_required(
    output: &PlannerOutput,
    required_test_targets_fn: &dyn Fn(&[String]) -> Vec<String>,
) -> Result<(), PlannerValidationError> {
    let processor =
        PlannerOutputProcessor::new("", std::iter::empty::<&str>(), required_test_targets_fn);
    processor.validate_tests_required(output)
}

/// Attempt to build a deterministic [`PlanOutput`] from `objective` without
/// calling the LLM planner.
///
/// Returns `Some(PlanOutput)` when the objective explicitly names exactly one
/// source code file that is not itself a test file. Returns `None` when the
/// fast path does not apply — the caller should fall back to the LLM planner.
///
/// `required_test_targets_fn` is called with the identified source file; any
/// returned paths become additional work tasks that depend on the source task.
/// When the function returns an empty list, only the source work task is created.
pub fn try_fast_plan(
    objective: &str,
    required_test_targets_fn: &dyn Fn(&[String]) -> Vec<String>,
) -> Option<PlanOutput> {
    PlannerOutputProcessor::new(
        objective,
        std::iter::empty::<&str>(),
        required_test_targets_fn,
    )
    .try_fast_plan()
}

/// Convert a validated [`PlannerOutput`] into a scheduler [`PlanOutput`].
///
/// Each task becomes a [`NodeRequest`] of kind `Work`. The planner-assigned
/// `id` is used as `NodeRequest.id`; `depends_on` entries are carried through
/// as `NodeId` values referencing siblings by their planner-local id.
///
/// The scheduler's `insert_children` rewrites those planner-local ids to
/// actual graph `NodeId`s at insertion time.
pub fn planner_output_to_plan_output(output: PlannerOutput) -> PlanOutput {
    PlannerOutputProcessor::into_plan_output(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Direct planner response parsing ─────────────────────────────────────────

    #[test]
    fn direct_planner_output_parses_successfully() {
        let json = r#"{"tasks":[{"id":"a","objective":"do alpha","operation":"modify","targets":["alpha.txt"],"depends_on":[]}]}"#;
        let result = try_parse_planner_response(json);
        assert!(
            result.is_ok(),
            "direct PlannerOutput JSON must parse; got {:?}",
            result
        );
        let output = result.unwrap();
        assert_eq!(output.tasks[0].id, "a");
    }

    #[test]
    fn planner_output_without_operation_field_parses_successfully() {
        // Regression: DefaultProjectAdapter's prompt schema (PLANNER_PROTOCOL_FOOTER)
        // never asks the model for `operation`, so a response following that
        // schema must not fail to parse for lacking a field the model was
        // never told to produce.
        let json = r#"{"tasks":[{"id":"a","objective":"do alpha","targets":["alpha.txt"],"depends_on":[]}]}"#;
        let result = try_parse_planner_response(json);
        assert!(
            result.is_ok(),
            "PlannerOutput without an operation field must parse; got {:?}",
            result
        );
        assert_eq!(result.unwrap().tasks[0].operation, None);
    }

    #[test]
    fn preamble_before_planner_json_is_rejected() {
        let result = try_parse_planner_response("Here is the plan:\n{\"tasks\":[]}");
        assert!(
            result.is_err(),
            "preamble before JSON must fail; got {:?}",
            result
        );
    }

    #[test]
    fn status_content_wrapper_fails_planner_parse() {
        let wrapped = r#"{"status":"accepted","content":"{\"tasks\":[]}"}"#;
        let result = try_parse_planner_response(wrapped);
        assert!(
            result.is_err(),
            "status/content wrapper must not parse as PlannerOutput; got {:?}",
            result
        );
    }

    // ── Parsing ─────────────────────────────────────────────────────────────────

    #[test]
    fn parses_multiple_tasks() {
        let json = r#"{
            "tasks": [
                {"id": "a", "objective": "do alpha", "operation": "modify", "targets": ["alpha.txt"], "depends_on": []},
                {"id": "b", "objective": "do beta", "operation": "modify", "targets": ["beta.txt"], "depends_on": []}
            ]
        }"#;
        let output = parse_planner_content(json).expect("parse must return Some");
        assert_eq!(output.tasks.len(), 2);
        assert_eq!(output.tasks[0].id, "a");
        assert_eq!(output.tasks[0].objective, "do alpha");
        assert_eq!(output.tasks[0].targets, vec!["alpha.txt"]);
        assert!(output.tasks[0].depends_on.is_empty());
        assert_eq!(output.tasks[1].id, "b");
    }

    #[test]
    fn parses_dependencies() {
        let json = r#"{
            "tasks": [
                {"id": "first",  "objective": "write tests", "operation": "modify", "targets": ["test.txt"], "depends_on": []},
                {"id": "second", "objective": "implement it", "operation": "modify", "targets": ["impl.txt"], "depends_on": ["first"]}
            ]
        }"#;
        let output = parse_planner_content(json).expect("parse must return Some");
        assert_eq!(output.tasks[1].depends_on, vec!["first"]);
    }

    #[test]
    fn prose_content_fails_parse() {
        let result = parse_planner_content("Just do the thing and make it work.");
        assert!(result.is_none(), "prose must not parse as PlannerOutput");
    }

    // ── Validation ──────────────────────────────────────────────────────────────

    fn planner_task(
        id: &str,
        objective: &str,
        targets: &[&str],
        depends_on: &[&str],
    ) -> PlannerTask {
        PlannerTask {
            id: id.to_string(),
            objective: objective.to_string(),
            operation: Some(PlannerOperation::Modify),
            targets: targets.iter().map(|s| s.to_string()).collect(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn structural_validation_rejects_invalid_plan() {
        let cases: &[(&str, PlannerOutput, PlannerValidationError)] = &[
            (
                "duplicate id",
                PlannerOutput {
                    tasks: vec![
                        planner_task("x", "first", &["first.txt"], &[]),
                        planner_task("x", "second", &["second.txt"], &[]),
                    ],
                },
                PlannerValidationError::DuplicateId("x".to_string()),
            ),
            (
                "empty objective",
                PlannerOutput {
                    tasks: vec![planner_task("task", "   ", &["task.txt"], &[])],
                },
                PlannerValidationError::EmptyObjective("task".to_string()),
            ),
            (
                "empty targets",
                PlannerOutput {
                    tasks: vec![planner_task("task", "do something", &[], &[])],
                },
                PlannerValidationError::EmptyTargets("task".to_string()),
            ),
            (
                "self dependency",
                PlannerOutput {
                    tasks: vec![planner_task(
                        "loop",
                        "do something",
                        &["loop.txt"],
                        &["loop"],
                    )],
                },
                PlannerValidationError::SelfDependency("loop".to_string()),
            ),
            (
                "unknown dependency",
                PlannerOutput {
                    tasks: vec![planner_task(
                        "task",
                        "do something",
                        &["task.txt"],
                        &["nonexistent"],
                    )],
                },
                PlannerValidationError::UnknownDependency {
                    task_id: "task".to_string(),
                    dep_id: "nonexistent".to_string(),
                },
            ),
        ];

        for (case, output, expected) in cases {
            let err = validate_planner_output(output)
                .expect_err(&format!("[{case}] validate_planner_output must return Err"));
            assert_eq!(err, *expected, "[{case}]");
        }
    }

    // ── No-recreate validation ───────────────────────────────────────────────────

    const PYTHON_INIT_FILES: &[&str] = &[
        ".gitignore",
        ".python-version",
        "README.md",
        "main.py",
        "pyproject.toml",
        "language.lock",
    ];

    #[test]
    fn task_targeting_existing_file_not_in_objective_is_rejected() {
        // Regression: planner created a task for an existing project file that
        // the objective (which only mentions main.py) never names — originally
        // ".python-version"; "README.md" is the same violation shape.
        let top_objective = "Create a simple Python program in main.py that prints a short haiku about Python state machines.";
        let cases: &[(&str, &str)] = &[("py-version", ".python-version"), ("readme", "README.md")];

        for (task_id, filename) in cases {
            let output = PlannerOutput {
                tasks: vec![PlannerTask {
                    id: task_id.to_string(),
                    objective: format!("Touch {filename}."),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec![filename.to_string()],
                    depends_on: vec![],
                }],
            };
            let err = validate_planner_no_recreate(&output, top_objective, PYTHON_INIT_FILES)
                .expect_err(&format!(
                    "[{task_id}] must reject task targeting {filename} not in objective"
                ));
            assert_eq!(
                err,
                PlannerValidationError::TaskRecreatesExistingFile {
                    task_id: task_id.to_string(),
                    filename: filename.to_string(),
                },
                "[{task_id}]"
            );
        }
    }

    #[test]
    fn task_for_objective_target_only_passes_no_recreate_validation() {
        // Only a main.py task — no infrastructure files touched.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "main".to_string(),
                objective: "Write a haiku about Python state machines in main.py.".to_string(),
                operation: Some(PlannerOperation::Modify),
                targets: vec!["main.py".to_string()],
                depends_on: vec![],
            }],
        };
        let top_objective = "Create a simple Python program in main.py that prints a short haiku about Python state machines.";
        assert!(
            validate_planner_no_recreate(&output, top_objective, PYTHON_INIT_FILES).is_ok(),
            "task targeting only main.py (which is in the objective) must pass"
        );
    }

    fn python_tests(targets: &[String]) -> Vec<String> {
        let rules = crate::language::language_spec("python")
            .expect("python language spec must load")
            .validation
            .validation_targets;
        crate::validation::derive_validation_targets(&rules, targets)
    }

    fn no_tests(_: &[String]) -> Vec<String> {
        vec![]
    }

    #[test]
    fn code_target_without_test_target_rejected_when_tests_required() {
        // Invariant: plan with only a source file fails when adapter requires a test file.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "main".to_string(),
                objective: "Modify main.py.".to_string(),
                operation: Some(PlannerOperation::Modify),
                targets: vec!["main.py".to_string()],
                depends_on: vec![],
            }],
        };
        assert_eq!(
            validate_planner_tests_required(&output, &python_tests),
            Err(PlannerValidationError::MissingTestsForCodeChange)
        );
    }

    #[test]
    fn code_target_with_test_target_passes_when_tests_required() {
        // Invariant: plan with source + adapter-required test file passes validation.
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        assert!(
            validate_planner_tests_required(&output, &python_tests).is_ok(),
            "main.py plus test_main.py must satisfy test-required planning"
        );
    }

    #[test]
    fn tests_required_passes_when_adapter_requires_nothing() {
        // Invariant: when the adapter returns no required tests, any plan passes.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "main".to_string(),
                objective: "Modify main.py.".to_string(),
                operation: Some(PlannerOperation::Modify),
                targets: vec!["main.py".to_string()],
                depends_on: vec![],
            }],
        };
        assert!(
            validate_planner_tests_required(&output, &no_tests).is_ok(),
            "plan-only task must pass when adapter requires no tests"
        );
    }

    #[test]
    fn explicit_objective_target_rejects_unlisted_non_exempt_target() {
        // Invariant: target not in objective and not in adapter exemptions is rejected.
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "config".to_string(),
                    objective: "Modify project config.".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["pyproject.toml".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: Some(PlannerOperation::Create),
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        let exempt = python_tests(&["main.py".to_string()]);
        let err =
            validate_planner_explicit_targets(&output, "Modify main.py to print a haiku.", &exempt)
                .expect_err("pyproject.toml must be rejected when only main.py is named");
        assert_eq!(
            err,
            PlannerValidationError::ExplicitTargetViolation {
                filename: "pyproject.toml".to_string(),
                allowed_targets: vec!["main.py".to_string()],
            }
        );
    }

    #[test]
    fn explicit_objective_target_allows_adapter_exempt_test_target() {
        // Invariant: adapter-provided test targets are exempt from the explicit-target check.
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: Some(PlannerOperation::Create),
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        let exempt = python_tests(&["main.py".to_string()]);
        assert!(
            validate_planner_explicit_targets(&output, "Modify main.py to print a haiku.", &exempt)
                .is_ok(),
            "test_main.py must be allowed as adapter-exempt test target"
        );
    }

    #[test]
    fn explicit_objective_target_no_exemptions_rejects_test_target() {
        // Invariant: when adapter provides no exemptions, test targets are not automatically allowed.
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: Some(PlannerOperation::Create),
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        let result =
            validate_planner_explicit_targets(&output, "Modify main.py to print a haiku.", &[]);
        assert!(
            result.is_err(),
            "test_main.py must be rejected when adapter provides no exemptions"
        );
    }

    #[test]
    fn no_recreate_allows_existing_file_when_objective_names_it() {
        // Objective explicitly targets pyproject.toml — planner task is allowed.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "config".to_string(),
                objective: "Update pyproject.toml to add a new dependency.".to_string(),
                operation: Some(PlannerOperation::Modify),
                targets: vec!["pyproject.toml".to_string()],
                depends_on: vec![],
            }],
        };
        let top_objective = "Add ruff to pyproject.toml as a dev dependency.";
        assert!(
            validate_planner_no_recreate(&output, top_objective, PYTHON_INIT_FILES).is_ok(),
            "task for pyproject.toml must pass when objective explicitly names it"
        );
    }

    #[test]
    fn no_recreate_empty_existing_files_always_passes() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "any".to_string(),
                objective: "Create anything at all.".to_string(),
                operation: Some(PlannerOperation::Modify),
                targets: vec!["anything.txt".to_string()],
                depends_on: vec![],
            }],
        };
        assert!(
            validate_planner_no_recreate(&output, "do something", &[] as &[&str]).is_ok(),
            "empty existing_files must always pass"
        );
    }

    // ── Mapping ─────────────────────────────────────────────────────────────────

    #[test]
    fn planner_tasks_become_node_requests() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "step-one".to_string(),
                    objective: "do step one".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["one.txt".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "step-two".to_string(),
                    objective: "do step two".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["two.txt".to_string()],
                    depends_on: vec![],
                },
            ],
        };
        let plan = planner_output_to_plan_output(output);
        assert_eq!(plan.children.len(), 2);
        assert_eq!(plan.children[0].id, NodeId("step-one".to_string()));
        assert_eq!(plan.children[0].kind, NodeKind::Work);
        assert_eq!(plan.children[0].objective, "do step one");
        assert_eq!(plan.children[0].target_files, vec!["one.txt".to_string()]);
        assert_eq!(plan.children[1].id, NodeId("step-two".to_string()));
        assert_eq!(plan.children[1].objective, "do step two");
        assert_eq!(plan.children[1].target_files, vec!["two.txt".to_string()]);
    }

    #[test]
    fn planner_dependencies_preserved() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "write tests".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["tests.txt".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "impl".to_string(),
                    objective: "implement".to_string(),
                    operation: Some(PlannerOperation::Modify),
                    targets: vec!["impl.txt".to_string()],
                    depends_on: vec!["tests".to_string()],
                },
            ],
        };
        let plan = planner_output_to_plan_output(output);
        assert_eq!(
            plan.children[1].dependencies,
            vec![NodeId("tests".to_string())]
        );
    }

    // ── try_fast_plan ────────────────────────────────────────────────────────────

    #[test]
    fn explicit_single_file_objective_produces_direct_plan() {
        // Invariant: single source file with no required tests yields one work task.
        let objective = "Create a simple Python program in main.py that prints a haiku.";
        let plan =
            try_fast_plan(objective, &no_tests).expect("must return Some for explicit single file");
        assert_eq!(plan.children.len(), 1, "no tests required → one work task");
        let child = &plan.children[0];
        assert_eq!(child.id, NodeId("work".to_string()));
        assert_eq!(child.kind, NodeKind::Work);
        assert!(child.objective.contains("main.py"));
        assert_eq!(child.target_files, vec!["main.py".to_string()]);
        assert!(
            child.dependencies.is_empty(),
            "work task must have no dependencies"
        );
    }

    #[test]
    fn explicit_single_file_with_adapter_tests_required_adds_test_target() {
        // Invariant: adapter-provided test target is appended as a dependent task.
        let objective = "Create a simple Python program in main.py that prints a haiku.";
        let plan = try_fast_plan(objective, &python_tests).expect("must return Some");
        assert_eq!(plan.children.len(), 2, "tests required → two work tasks");

        let work = &plan.children[0];
        assert_eq!(work.id, NodeId("work".to_string()));
        assert_eq!(work.target_files, vec!["main.py".to_string()]);

        let tests = &plan.children[1];
        assert_eq!(tests.id, NodeId("tests".to_string()));
        assert_eq!(tests.target_files, vec!["test_main.py".to_string()]);
        assert_eq!(
            tests.dependencies,
            vec![NodeId("work".to_string())],
            "test task must depend on work task"
        );
    }

    #[test]
    fn objective_without_explicit_file_falls_back_to_planner() {
        // Invariant: objective without a named file returns None.
        let plan = try_fast_plan("Refactor the error handling in the codebase.", &no_tests);
        assert!(
            plan.is_none(),
            "objective without a named file must return None"
        );
    }

    #[test]
    fn objective_with_multiple_explicit_files_falls_back_to_planner() {
        // Invariant: objective naming two source files returns None (LLM planner needed).
        let plan = try_fast_plan("Modify main.py and utils.py to add logging.", &no_tests);
        assert!(
            plan.is_none(),
            "objective naming two source files must return None, not a fast plan"
        );
    }

    #[test]
    fn explicit_test_file_in_objective_does_not_trigger_fast_plan() {
        // Invariant: a test-file-only objective must not trigger the fast path.
        let plan = try_fast_plan("Add assertions to test_main.py.", &no_tests);
        assert!(
            plan.is_none(),
            "a test-file-only objective must not trigger the fast path"
        );
    }

    #[test]
    fn fast_plan_uses_adapter_fn_for_test_targets() {
        // Invariant: fast plan delegates test file naming to the adapter fn, not hardcoding.
        let custom_fn = |sources: &[String]| -> Vec<String> {
            sources
                .iter()
                .filter(|s| s.ends_with(".py"))
                .map(|s| {
                    let stem = s.trim_end_matches(".py");
                    format!("custom_test_{stem}.py")
                })
                .collect()
        };
        let objective = "Create main.py with a greeting function.";
        let plan = try_fast_plan(objective, &custom_fn).expect("must return Some");
        assert_eq!(plan.children.len(), 2);
        assert!(
            plan.children
                .iter()
                .any(|c| c.target_files == vec!["custom_test_main.py".to_string()]),
            "fast plan must use adapter fn for test target naming; got: {:?}",
            plan.children
                .iter()
                .map(|c| &c.target_files)
                .collect::<Vec<_>>()
        );
    }
}
