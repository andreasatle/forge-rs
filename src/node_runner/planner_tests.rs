use super::*;

fn processor<'a>(
    top_objective: &str,
    existing_files: &'a [&'a str],
    required_test_targets_fn: &'a dyn Fn(&[String]) -> Vec<String>,
) -> PlannerOutputProcessor<'a> {
    PlannerOutputProcessor::new(top_objective, existing_files, required_test_targets_fn)
}

fn parse_planner_content(content: &str) -> Option<PlannerOutput> {
    processor("", &[], &no_required_test_targets).parse_content(content)
}

fn try_parse_planner_response(raw: &str) -> Result<PlannerOutput, String> {
    processor("", &[], &no_required_test_targets).parse_response(raw)
}

fn validate_planner_output(output: &PlannerOutput) -> Result<(), PlannerValidationError> {
    processor("", &[], &no_required_test_targets).validate(output)
}

fn validate_planner_explicit_targets(
    output: &PlannerOutput,
    top_objective: &str,
    exempt_targets: &[String],
) -> Result<(), PlannerValidationError> {
    processor(top_objective, &[], &no_required_test_targets)
        .validate_explicit_targets(output, exempt_targets)
}

fn validate_planner_no_recreate(
    output: &PlannerOutput,
    top_objective: &str,
    existing_files: &[&str],
) -> Result<(), PlannerValidationError> {
    processor(top_objective, existing_files, &no_required_test_targets).validate_no_recreate(output)
}

fn validate_planner_tests_required(
    output: &PlannerOutput,
    required_test_targets_fn: &dyn Fn(&[String]) -> Vec<String>,
) -> Result<(), PlannerValidationError> {
    processor("", &[], required_test_targets_fn).validate_tests_required(output)
}

fn planner_output_to_plan_output(output: PlannerOutput) -> PlanOutput {
    processor("", &[], &no_required_test_targets).into_plan(output)
}

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
    let json =
        r#"{"tasks":[{"id":"a","objective":"do alpha","targets":["alpha.txt"],"depends_on":[]}]}"#;
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

fn planner_task(id: &str, objective: &str, targets: &[&str], depends_on: &[&str]) -> PlannerTask {
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
