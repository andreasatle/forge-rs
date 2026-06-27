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
    /// Ids of other tasks in the same output that must complete before this one.
    pub depends_on: Vec<String>,
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
    /// Two tasks share the same id.
    DuplicateId(String),
    /// A task has an empty (or whitespace-only) objective.
    EmptyObjective(String),
    /// A task lists its own id in `depends_on`.
    SelfDependency(String),
    /// A task's `depends_on` references an id not present in the output.
    UnknownDependency {
        /// The id of the task containing the invalid reference.
        task_id: String,
        /// The unknown dependency id that was referenced.
        dep_id: String,
    },
    /// A task's objective references an existing project file that is not
    /// mentioned in the top-level run objective, indicating the planner is
    /// trying to recreate an infrastructure file it should leave alone.
    TaskRecreatesExistingFile {
        /// The task id whose objective contains the pre-existing filename.
        task_id: String,
        /// The filename that already exists and is not an objective target.
        filename: String,
    },
}

impl std::fmt::Display for PlannerValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlannerValidationError::DuplicateId(id) => {
                write!(f, "duplicate task id: {id}")
            }
            PlannerValidationError::EmptyObjective(id) => {
                write!(f, "empty objective for task: {id}")
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
/// - No task lists itself in `depends_on`.
/// - Every `depends_on` entry names another task in the same output.
///
/// Returns `Err` on the first violation. Does not attempt to repair.
pub fn validate_planner_output(output: &PlannerOutput) -> Result<(), PlannerValidationError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for task in &output.tasks {
        if !seen.insert(task.id.as_str()) {
            return Err(PlannerValidationError::DuplicateId(task.id.clone()));
        }
        if task.objective.trim().is_empty() {
            return Err(PlannerValidationError::EmptyObjective(task.id.clone()));
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

/// Check that no task in `output` targets an existing project file that is not
/// mentioned in `top_objective`.
///
/// A task is considered to target an existing file when its objective string
/// contains a filename from `existing_files` AND that filename does not appear
/// in `top_objective`. This catches planners that recreate infrastructure files
/// (e.g. `.python-version`, `pyproject.toml`) when the objective only targets
/// `main.py`.
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
            if task.objective.contains(filename) && !top_objective.contains(filename) {
                return Err(PlannerValidationError::TaskRecreatesExistingFile {
                    task_id: task.id.clone(),
                    filename: filename.to_string(),
                });
            }
        }
    }
    Ok(())
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
        let json = r#"{"tasks":[{"id":"a","objective":"do alpha","depends_on":[]}]}"#;
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
        let direct = r#"{"tasks":[{"id":"t1","objective":"do the work","depends_on":[]}]}"#;
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
                {"id": "a", "objective": "do alpha", "depends_on": []},
                {"id": "b", "objective": "do beta",  "depends_on": []}
            ]
        }"#;
        let output = parse_planner_content(json).expect("parse must return Some");
        assert_eq!(output.tasks.len(), 2);
        assert_eq!(output.tasks[0].id, "a");
        assert_eq!(output.tasks[0].objective, "do alpha");
        assert!(output.tasks[0].depends_on.is_empty());
        assert_eq!(output.tasks[1].id, "b");
    }

    #[test]
    fn parses_dependencies() {
        let json = r#"{
            "tasks": [
                {"id": "first",  "objective": "write tests",  "depends_on": []},
                {"id": "second", "objective": "implement it", "depends_on": ["first"]}
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
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "x".to_string(),
                    objective: "second".to_string(),
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
    fn self_dependency_rejected() {
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "loop".to_string(),
                objective: "do something".to_string(),
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
        "uv.lock",
    ];

    #[test]
    fn task_targeting_existing_file_not_in_objective_is_rejected() {
        // Regression: planner created ".python-version" task for objective that only mentions main.py.
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "py-version".to_string(),
                    objective: "Create .python-version file for Python project.".to_string(),
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "main".to_string(),
                    objective: "Create main.py with haiku about Python state machines.".to_string(),
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
    fn task_for_objective_target_only_passes_no_recreate_validation() {
        // Only a main.py task — no infrastructure files touched.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "main".to_string(),
                objective: "Write a haiku about Python state machines in main.py.".to_string(),
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
    fn no_recreate_allows_existing_file_when_objective_names_it() {
        // Objective explicitly targets pyproject.toml — planner task is allowed.
        let output = PlannerOutput {
            tasks: vec![PlannerTask {
                id: "config".to_string(),
                objective: "Update pyproject.toml to add a new dependency.".to_string(),
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
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "step-two".to_string(),
                    objective: "do step two".to_string(),
                    depends_on: vec![],
                },
            ],
        };
        let plan = planner_output_to_plan_output(output);
        assert_eq!(plan.children.len(), 2);
        assert_eq!(plan.children[0].id, NodeId("step-one".to_string()));
        assert_eq!(plan.children[0].kind, NodeKind::Work);
        assert_eq!(plan.children[0].objective, "do step one");
        assert_eq!(plan.children[1].id, NodeId("step-two".to_string()));
    }

    #[test]
    fn planner_dependencies_preserved() {
        let output = PlannerOutput {
            tasks: vec![
                PlannerTask {
                    id: "tests".to_string(),
                    objective: "write tests".to_string(),
                    depends_on: vec![],
                },
                PlannerTask {
                    id: "impl".to_string(),
                    objective: "implement".to_string(),
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
}
