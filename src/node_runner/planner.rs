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
    /// Concrete artifact operation this task will perform.
    pub operation: PlannerOperation,
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

/// Attempt to parse raw provider content as a [`PlannerOutput`].
///
/// Returns `Some(PlannerOutput)` on success, `None` if the content cannot be
/// parsed. A parse failure is not an error in the run — prose output is an
/// expected fallback case that triggers single-work-node behaviour.
pub fn parse_planner_content(content: &str) -> Option<PlannerOutput> {
    serde_json::from_str::<PlannerOutput>(content).ok()
}

/// Parse a raw provider response as a [`PlannerOutput`] directly.
///
/// Unlike [`parse_planner_content`] this returns a `Result` suitable for the
/// role runner's retry path. A preamble before the opening `{` is rejected
/// immediately without attempting JSON parsing.
pub fn try_parse_planner_response(raw: &str) -> Result<PlannerOutput, String> {
    let text = raw.trim();
    if !text.starts_with('{') {
        return Err(
            "planner response must start with '{'; preamble text is not permitted".to_string(),
        );
    }
    serde_json::from_str::<PlannerOutput>(text)
        .map_err(|e| format!("planner JSON parse error: {e}"))
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

/// Check that when a coding objective explicitly names target files, all
/// non-test planner targets are among those named files.
///
/// Test targets are intentionally exempt because project validation can require
/// newly-created tests even when the user only names the implementation file.
pub fn validate_planner_explicit_targets(
    output: &PlannerOutput,
    top_objective: &str,
) -> Result<(), PlannerValidationError> {
    let allowed_targets = explicit_objective_targets(top_objective);
    if allowed_targets.is_empty()
        || !allowed_targets
            .iter()
            .any(|target| target_is_code_like(target))
    {
        return Ok(());
    }

    for target in output.tasks.iter().flat_map(|task| task.targets.iter()) {
        let normalized = normalize_target_token(target);
        if !target_is_test_related(&normalized) && !allowed_targets.contains(&normalized) {
            return Err(PlannerValidationError::ExplicitTargetViolation {
                filename: normalized,
                allowed_targets: sorted_targets(&allowed_targets),
            });
        }
    }

    Ok(())
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
    for task in &output.tasks {
        for filename in existing_files {
            let filename = filename.as_ref();
            if task.targets.iter().any(|target| target == filename)
                && !top_objective.contains(filename)
            {
                return Err(PlannerValidationError::TaskRecreatesExistingFile {
                    task_id: task.id.clone(),
                    filename: filename.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Check that a code-changing plan includes at least one test-related target
/// when project validation includes a test command.
///
/// This is intentionally based on structured `targets`, not objective prose.
/// A target is considered code-like when it has a common source-file extension.
/// A target is considered test-related when its path or filename clearly names
/// tests. The language-specific reason tests are required comes from the
/// configured validation commands; this helper does not inspect language IDs.
pub fn validate_planner_tests_required(
    output: &PlannerOutput,
) -> Result<(), PlannerValidationError> {
    let has_code_target = output
        .tasks
        .iter()
        .flat_map(|task| task.targets.iter())
        .any(|target| target_is_code_like(target) && !target_is_test_related(target));
    if !has_code_target {
        return Ok(());
    }

    let has_test_target = output
        .tasks
        .iter()
        .flat_map(|task| task.targets.iter())
        .any(|target| target_is_test_related(target));
    if has_test_target {
        Ok(())
    } else {
        Err(PlannerValidationError::MissingTestsForCodeChange)
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

fn explicit_objective_targets(top_objective: &str) -> HashSet<String> {
    top_objective
        .split_whitespace()
        .map(normalize_target_token)
        .filter(|token| token_contains_file_separator(token) && token_has_file_extension(token))
        .collect()
}

fn normalize_target_token(token: &str) -> String {
    token
        .trim_matches(|c: char| {
            !(c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '\\' | '_' | '-' | '@' | '+'))
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

fn sorted_targets(targets: &HashSet<String>) -> Vec<String> {
    let mut sorted: Vec<String> = targets.iter().cloned().collect();
    sorted.sort();
    sorted
}

/// Attempt to build a deterministic [`PlanOutput`] from `objective` without
/// calling the LLM planner.
///
/// Returns `Some(PlanOutput)` when the objective explicitly names exactly one
/// source code file that is not itself a test file. Returns `None` when the
/// fast path does not apply — the caller should fall back to the LLM planner.
///
/// When `requires_tests` is true and the fast path applies, a second work
/// task targeting the conventionally-derived test file is appended with a
/// dependency on the source work task.
pub fn try_fast_plan(objective: &str, requires_tests: bool) -> Option<PlanOutput> {
    let all_targets = explicit_objective_targets(objective);
    let mut source_targets: Vec<String> = all_targets
        .into_iter()
        .filter(|t| target_is_code_like(t) && !target_is_test_related(t))
        .collect();
    source_targets.sort();

    if source_targets.len() != 1 {
        return None;
    }

    let source = source_targets.into_iter().next().unwrap();
    let work = NodeRequest {
        id: NodeId("work".to_string()),
        kind: NodeKind::Work,
        objective: objective.to_string(),
        target_files: vec![source.clone()],
        dependencies: vec![],
    };
    let mut children = vec![work];

    if requires_tests {
        let test_target = derive_test_target(&source);
        let tests = NodeRequest {
            id: NodeId("tests".to_string()),
            kind: NodeKind::Work,
            objective: format!(
                "Write tests that verify the work described by the following objective:\n\n\
                 {objective}"
            ),
            target_files: vec![test_target],
            dependencies: vec![NodeId("work".to_string())],
        };
        children.push(tests);
    }

    Some(PlanOutput { children })
}

/// Derive a conventional test file path for a given source file.
///
/// Uses extension-based conventions; does not consult any language runtime.
/// Falls back to `test_{filename}` for extensions not explicitly listed.
fn derive_test_target(source: &str) -> String {
    let path = source.replace('\\', "/");
    let (prefix, filename) = path
        .rsplit_once('/')
        .map(|(dir, f)| (format!("{dir}/"), f))
        .unwrap_or(("".into(), path.as_str()));
    let Some((stem, ext)) = filename.rsplit_once('.') else {
        return format!("{prefix}test_{filename}");
    };
    let lower = ext.to_ascii_lowercase();
    match lower.as_str() {
        "go" | "rs" => format!("{prefix}{stem}_test.{ext}"),
        "js" | "ts" | "jsx" | "tsx" => format!("{prefix}{stem}.test.{ext}"),
        _ => format!("{prefix}test_{stem}.{ext}"),
    }
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
    PlanOutput {
        children: output
            .tasks
            .into_iter()
            .map(|task| NodeRequest {
                id: NodeId(task.id),
                kind: NodeKind::Work,
                objective: task.objective,
                target_files: task.targets,
                dependencies: task.depends_on.into_iter().map(NodeId).collect(),
            })
            .collect(),
    }
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
    fn planner_output_does_not_require_nested_json_string() {
        // The planner schema is {"tasks":[...]} directly, not wrapped in
        // {"status":"accepted","content":"<escaped-json>"}.
        let direct = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
        assert!(
            try_parse_planner_response(direct).is_ok(),
            "direct PlannerOutput must parse without a status/content wrapper"
        );
    }

    #[test]
    fn planner_does_not_require_content_string_starting_with_brace() {
        // Regression: live failure produced {"status":"accepted","content":"{"}
        // which must fail cleanly, not panic or produce PlanAccepted.
        let payload = r#"{"status":"accepted","content":"{"}"#;
        let result = try_parse_planner_response(payload);
        assert!(
            result.is_err(),
            "status/content wrapper must not parse as PlannerOutput; got {:?}",
            result
        );
    }

    #[test]
    fn preamble_before_planner_json_is_rejected() {
        let result = try_parse_planner_response("Here is the plan:\n{\"tasks\":[]}");
        assert!(
            result.is_err(),
            "preamble before JSON must fail; got {:?}",
            result
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("preamble text is not permitted"),
            "error must mention preamble; got: {err}"
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

    #[test]
    fn duplicate_ids_rejected() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "x".to_string(),
                    objective: "first".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["first.txt".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "x".to_string(),
                    objective: "second".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["second.txt".to_string()],
                    depends_on: vec![],
                },
            ],
        };
        let err = validate_planner_output(&output).unwrap_err();
        assert_eq!(err, PlannerValidationError::DuplicateId("x".to_string()));
    }

    #[test]
    fn empty_objective_rejected() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "task".to_string(),
                objective: "   ".to_string(),
                operation: PlannerOperation::Modify,
                targets: vec!["task.txt".to_string()],
                depends_on: vec![],
            }],
        };
        let err = validate_planner_output(&output).unwrap_err();
        assert_eq!(
            err,
            PlannerValidationError::EmptyObjective("task".to_string())
        );
    }

    #[test]
    fn empty_targets_rejected() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "task".to_string(),
                objective: "do something".to_string(),
                operation: PlannerOperation::Modify,
                targets: vec![],
                depends_on: vec![],
            }],
        };
        let err = validate_planner_output(&output).unwrap_err();
        assert_eq!(
            err,
            PlannerValidationError::EmptyTargets("task".to_string())
        );
    }

    #[test]
    fn self_dependency_rejected() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "loop".to_string(),
                objective: "do something".to_string(),
                operation: PlannerOperation::Modify,
                targets: vec!["loop.txt".to_string()],
                depends_on: vec!["loop".to_string()],
            }],
        };
        let err = validate_planner_output(&output).unwrap_err();
        assert_eq!(
            err,
            PlannerValidationError::SelfDependency("loop".to_string())
        );
    }

    #[test]
    fn unknown_dependency_rejected() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "task".to_string(),
                objective: "do something".to_string(),
                operation: PlannerOperation::Modify,
                targets: vec!["task.txt".to_string()],
                depends_on: vec!["nonexistent".to_string()],
            }],
        };
        let err = validate_planner_output(&output).unwrap_err();
        assert_eq!(
            err,
            PlannerValidationError::UnknownDependency {
                task_id: "task".to_string(),
                dep_id: "nonexistent".to_string(),
            }
        );
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
        // Regression: planner created ".python-version" task for objective that only mentions main.py.
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "py-version".to_string(),
                    objective: "Create .python-version file for Python project.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec![".python-version".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Create main.py with haiku about Python state machines.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
            ],
        };
        let top_objective = "Create a simple Python program in main.py that prints a short haiku about Python state machines.";
        let result = validate_planner_no_recreate(&output, top_objective, PYTHON_INIT_FILES);
        let err = result.expect_err("must reject task targeting .python-version not in objective");
        assert!(
            matches!(
                err,
                PlannerValidationError::TaskRecreatesExistingFile {
                    ref task_id,
                    ref filename,
                } if task_id == "py-version" && filename == ".python-version"
            ),
            "expected TaskRecreatesExistingFile for .python-version; got {err:?}"
        );
    }

    #[test]
    fn task_targeting_readme_for_main_objective_is_rejected() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "readme".to_string(),
                objective: "Document the haiku program setup.".to_string(),
                operation: PlannerOperation::Modify,
                targets: vec!["README.md".to_string()],
                depends_on: vec![],
            }],
        };
        let top_objective = "Create a simple Python program in main.py that prints a short haiku about Python state machines.";
        let err = validate_planner_no_recreate(&output, top_objective, PYTHON_INIT_FILES)
            .expect_err("README.md target must be rejected when only main.py is requested");
        assert_eq!(
            err,
            PlannerValidationError::TaskRecreatesExistingFile {
                task_id: "readme".to_string(),
                filename: "README.md".to_string(),
            }
        );
    }

    #[test]
    fn task_for_objective_target_only_passes_no_recreate_validation() {
        // Only a main.py task — no infrastructure files touched.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "main".to_string(),
                objective: "Write a haiku about Python state machines in main.py.".to_string(),
                operation: PlannerOperation::Modify,
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

    #[test]
    fn code_target_without_test_target_rejected_when_tests_required() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "main".to_string(),
                objective: "Modify main.py.".to_string(),
                operation: PlannerOperation::Modify,
                targets: vec!["main.py".to_string()],
                depends_on: vec![],
            }],
        };
        assert_eq!(
            validate_planner_tests_required(&output),
            Err(PlannerValidationError::MissingTestsForCodeChange)
        );
    }

    #[test]
    fn code_target_with_test_target_passes_when_tests_required() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        assert!(
            validate_planner_tests_required(&output).is_ok(),
            "main.py plus test_main.py must satisfy test-required planning"
        );
    }

    #[test]
    fn explicit_objective_target_rejects_unlisted_non_test_target() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "config".to_string(),
                    objective: "Modify project config.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["pyproject.toml".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: PlannerOperation::Create,
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        let err = validate_planner_explicit_targets(&output, "Modify main.py to print a haiku.")
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
    fn explicit_objective_target_allows_test_target() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Modify main.py.".to_string(),
                    operation: PlannerOperation::Modify,
                    targets: vec!["main.py".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "Add tests for main.py.".to_string(),
                    operation: PlannerOperation::Create,
                    targets: vec!["test_main.py".to_string()],
                    depends_on: vec!["main".to_string()],
                },
            ],
        };
        assert!(
            validate_planner_explicit_targets(&output, "Modify main.py to print a haiku.").is_ok(),
            "test_main.py must be allowed as a test target"
        );
    }

    #[test]
    fn no_recreate_allows_existing_file_when_objective_names_it() {
        // Objective explicitly targets pyproject.toml — planner task is allowed.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "config".to_string(),
                objective: "Update pyproject.toml to add a new dependency.".to_string(),
                operation: PlannerOperation::Modify,
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
                operation: PlannerOperation::Modify,
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
                    operation: PlannerOperation::Modify,
                    targets: vec!["one.txt".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "step-two".to_string(),
                    objective: "do step two".to_string(),
                    operation: PlannerOperation::Modify,
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
                    operation: PlannerOperation::Modify,
                    targets: vec!["tests.txt".to_string()],
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "impl".to_string(),
                    objective: "implement".to_string(),
                    operation: PlannerOperation::Modify,
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
        let objective = "Create a simple Python program in main.py that prints a haiku.";
        let plan =
            try_fast_plan(objective, false).expect("must return Some for explicit single file");
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
    fn explicit_single_file_with_tests_required_adds_test_target() {
        let objective = "Create a simple Python program in main.py that prints a haiku.";
        let plan = try_fast_plan(objective, true).expect("must return Some");
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
        let plan = try_fast_plan("Refactor the error handling in the codebase.", false);
        assert!(
            plan.is_none(),
            "objective without a named file must return None"
        );
    }

    #[test]
    fn objective_with_multiple_explicit_files_falls_back_to_planner() {
        let plan = try_fast_plan("Modify main.py and utils.py to add logging.", false);
        assert!(
            plan.is_none(),
            "objective naming two source files must return None, not a fast plan"
        );
    }

    #[test]
    fn fast_plan_derives_correct_test_targets_by_extension() {
        let cases = [
            ("main.py", "test_main.py"),
            ("server.go", "server_test.go"),
            ("lib.rs", "lib_test.rs"),
            ("util.js", "util.test.js"),
            ("component.ts", "component.test.ts"),
            ("widget.tsx", "widget.test.tsx"),
            ("app.jsx", "app.test.jsx"),
            ("helper.rb", "test_helper.rb"),
        ];
        for (source, expected_test) in cases {
            assert_eq!(
                derive_test_target(source),
                expected_test,
                "wrong test target for {source}"
            );
        }
    }

    #[test]
    fn fast_plan_preserves_directory_prefix_in_test_target() {
        assert_eq!(derive_test_target("src/main.py"), "src/test_main.py");
        assert_eq!(derive_test_target("pkg/server.go"), "pkg/server_test.go");
        assert_eq!(derive_test_target("lib/util.rs"), "lib/util_test.rs");
    }

    #[test]
    fn explicit_test_file_in_objective_does_not_trigger_fast_plan() {
        // If the only explicitly named file is a test file, it is not a source target.
        let plan = try_fast_plan("Add assertions to test_main.py.", false);
        assert!(
            plan.is_none(),
            "a test-file-only objective must not trigger the fast path"
        );
    }
}
