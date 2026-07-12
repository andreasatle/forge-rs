use super::*;

fn processor(
    required_test_targets_fn: &dyn Fn(&[String]) -> Vec<String>,
) -> PlannerOutputProcessor<'_> {
    PlannerOutputProcessor::new(required_test_targets_fn, &[])
}

fn processor_with_roles<'a>(
    required_test_targets_fn: &'a dyn Fn(&[String]) -> Vec<String>,
    available_worker_roles: &'a [(String, String)],
) -> PlannerOutputProcessor<'a> {
    PlannerOutputProcessor::new(required_test_targets_fn, available_worker_roles)
}

fn parse_planner_content(content: &str) -> Option<PlannerOutput> {
    processor(&no_required_test_targets).parse_content(content)
}

fn try_parse_planner_response(raw: &str) -> Result<PlannerOutput, String> {
    processor(&no_required_test_targets).parse_response(raw)
}

fn validate_planner_output(output: &PlannerOutput) -> Result<(), PlannerValidationError> {
    processor(&no_required_test_targets).validate(output)
}

fn validate_planner_tests_required(
    output: &PlannerOutput,
    required_test_targets_fn: &dyn Fn(&[String]) -> Vec<String>,
) -> Result<(), PlannerValidationError> {
    processor(required_test_targets_fn).validate_tests_required(output)
}

fn planner_output_to_plan_output(output: PlannerOutput) -> PlanOutput {
    processor(&no_required_test_targets).into_plan(
        output,
        String::new(),
        String::new(),
        String::new(),
    )
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

// ── Recursive planning: `kind` field (Plan-node `PlannerOutput`) ────────────

#[test]
fn missing_kind_field_defaults_to_work() {
    // Invariant: planners that predate recursive planning omit `kind`
    // entirely; their output must still parse and behave as `Work`.
    let json =
        r#"{"tasks":[{"id":"a","objective":"do alpha","targets":["alpha.txt"],"depends_on":[]}]}"#;
    let output = parse_planner_content(json).expect("parse must return Some");
    assert_eq!(output.kind, PlannerOutputKind::Work);
}

#[test]
fn explicit_plan_kind_parses_with_empty_targets() {
    // Invariant: `kind: "plan"` tasks have no concrete files yet, so an empty
    // `targets` array must parse successfully rather than being rejected at
    // the JSON level.
    let json = r#"{"kind":"plan","tasks":[{"id":"a","objective":"decompose alpha","targets":[],"depends_on":[]}]}"#;
    let output = parse_planner_content(json).expect("parse must return Some");
    assert_eq!(output.kind, PlannerOutputKind::Plan);
    assert!(output.tasks[0].targets.is_empty());
}

#[test]
fn explicit_task_kind_parses_with_no_targets() {
    // Invariant: `kind: "task"` tasks are pure planner intent (id, objective,
    // depends_on) and carry no targets, so omitting `targets` entirely must
    // parse successfully.
    let json =
        r#"{"kind":"task","tasks":[{"id":"a","objective":"decompose alpha","depends_on":[]}]}"#;
    let output = parse_planner_content(json).expect("parse must return Some");
    assert_eq!(output.kind, PlannerOutputKind::Task);
    assert!(output.tasks[0].targets.is_empty());
}

// ── Validation (Plan-node `PlannerOutput`) ──────────────────────────────────

fn planner_task(id: &str, objective: &str, targets: &[&str], depends_on: &[&str]) -> PlannerTask {
    PlannerTask {
        id: id.to_string(),
        objective: objective.to_string(),
        name: String::new(),
        operation: Some(PlannerOperation::Modify),
        role: None,
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
                kind: PlannerOutputKind::Work,
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
                kind: PlannerOutputKind::Work,
                tasks: vec![planner_task("task", "   ", &["task.txt"], &[])],
            },
            PlannerValidationError::EmptyObjective("task".to_string()),
        ),
        (
            "empty targets",
            PlannerOutput {
                kind: PlannerOutputKind::Work,
                tasks: vec![planner_task("task", "do something", &[], &[])],
            },
            PlannerValidationError::EmptyTargets("task".to_string()),
        ),
        (
            "self dependency",
            PlannerOutput {
                kind: PlannerOutputKind::Work,
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
                kind: PlannerOutputKind::Work,
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

#[test]
fn work_task_missing_role_rejected_when_adapter_defines_worker_roles() {
    // Invariant: when the adapter defines worker roles, every work task must
    // be assigned one of them; an unassigned task fails validation.
    let roles = [("implementer".to_string(), "Implements code.".to_string())];
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![planner_task("task", "do something", &["task.txt"], &[])],
    };
    let err = processor_with_roles(&no_required_test_targets, &roles)
        .validate(&output)
        .expect_err("task with no role must fail validation when roles are configured");
    assert_eq!(
        err,
        PlannerValidationError::MissingTaskRole {
            task_id: "task".to_string()
        }
    );
}

#[test]
fn work_task_with_unknown_role_rejected_when_adapter_defines_worker_roles() {
    // Invariant: a role that does not match any configured worker role name
    // is rejected the same as a missing one.
    let roles = [("implementer".to_string(), "Implements code.".to_string())];
    let mut output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![planner_task("task", "do something", &["task.txt"], &[])],
    };
    output.tasks[0].role = Some("nonexistent-role".to_string());
    let err = processor_with_roles(&no_required_test_targets, &roles)
        .validate(&output)
        .expect_err("task with unrecognized role must fail validation");
    assert_eq!(
        err,
        PlannerValidationError::MissingTaskRole {
            task_id: "task".to_string()
        }
    );
}

#[test]
fn work_task_with_valid_role_passes_when_adapter_defines_worker_roles() {
    // Invariant: a task assigned one of the adapter's configured worker
    // roles passes validation.
    let roles = [("implementer".to_string(), "Implements code.".to_string())];
    let mut output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![planner_task("task", "do something", &["task.txt"], &[])],
    };
    output.tasks[0].role = Some("implementer".to_string());
    assert!(
        processor_with_roles(&no_required_test_targets, &roles)
            .validate(&output)
            .is_ok(),
        "task with a valid role must pass validation"
    );
}

#[test]
fn missing_role_not_enforced_when_adapter_defines_no_worker_roles() {
    // Invariant: when the adapter defines no worker roles (e.g.
    // DefaultProjectAdapter), role assignment stays optional — unchanged
    // behavior from before worker roles existed.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![planner_task("task", "do something", &["task.txt"], &[])],
    };
    assert!(
        validate_planner_output(&output).is_ok(),
        "task with no role must pass validation when no worker roles are configured"
    );
}

#[test]
fn plan_kind_task_missing_role_skips_role_validation() {
    // Invariant: `kind: "plan"` tasks are exempt from role validation, the
    // same as target validation — escalated tasks have no concrete targets
    // or role yet.
    let roles = [("implementer".to_string(), "Implements code.".to_string())];
    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![planner_task("sub-plan", "decompose this further", &[], &[])],
    };
    assert!(
        processor_with_roles(&no_required_test_targets, &roles)
            .validate(&output)
            .is_ok(),
        "plan-kind task must not require a role even when worker roles are configured"
    );
}

#[test]
fn plan_kind_task_with_empty_targets_passes_structural_validation() {
    // Invariant: `kind: "plan"` tasks have no concrete files yet, so an empty
    // `targets` array must not trigger `EmptyTargets`.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![planner_task("sub-plan", "decompose this further", &[], &[])],
    };
    assert!(
        validate_planner_output(&output).is_ok(),
        "plan-kind task with empty targets must pass validation"
    );
}

#[test]
fn plan_kind_task_still_requires_non_empty_objective() {
    // Invariant: the `kind` field only exempts target-related validation —
    // structural checks like a non-empty objective still apply.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![planner_task("sub-plan", "   ", &[], &[])],
    };
    assert_eq!(
        validate_planner_output(&output),
        Err(PlannerValidationError::EmptyObjective(
            "sub-plan".to_string()
        ))
    );
}

#[test]
fn task_kind_task_with_blank_name_fails_validation() {
    // Invariant: `kind: "task"` tasks must carry a non-blank `name` — the
    // grammar requires the field, so a blank value is a validation failure
    // rather than a silently accepted default.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Task,
        tasks: vec![PlannerTask {
            id: "sub-a".to_string(),
            objective: "decompose part a".to_string(),
            name: "  ".to_string(),
            operation: None,
            role: None,
            targets: vec![],
            depends_on: vec![],
        }],
    };
    assert_eq!(
        validate_planner_output(&output),
        Err(PlannerValidationError::EmptyName("sub-a".to_string()))
    );
}

#[test]
fn task_kind_task_with_name_passes_validation() {
    // Invariant: a non-blank `name` on a `kind: "task"` task satisfies the
    // requirement checked by `task_kind_task_with_blank_name_fails_validation`.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Task,
        tasks: vec![PlannerTask {
            id: "sub-a".to_string(),
            objective: "decompose part a".to_string(),
            name: "fibonacci".to_string(),
            operation: None,
            role: None,
            targets: vec![],
            depends_on: vec![],
        }],
    };
    assert!(
        validate_planner_output(&output).is_ok(),
        "task-kind task with a non-blank name must pass validation"
    );
}

#[test]
fn plan_kind_skips_tests_required_check() {
    // Invariant: `kind: "plan"` tasks are exempt from target-based validation
    // entirely. This task's target shape — no test target — would fail the
    // tests-required check under `kind: "work"` (see other tests in this
    // module); under `kind: "plan"` it must pass.
    fn required_test_targets(_: &[String]) -> Vec<String> {
        vec!["tests/test_main.py".to_string()]
    }

    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![planner_task(
            "sub-plan",
            "decompose the pyproject.toml change",
            &["pyproject.toml"],
            &[],
        )],
    };
    let processor = PlannerOutputProcessor::new(&required_test_targets, &[]);
    assert!(
        processor.validate(&output).is_ok(),
        "plan-kind output must skip the tests-required check"
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
        kind: PlannerOutputKind::Work,
        tasks: vec![PlannerTask {
            id: "main".to_string(),
            objective: "Modify main.py.".to_string(),
            name: String::new(),
            operation: Some(PlannerOperation::Modify),
            role: None,
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
        kind: PlannerOutputKind::Work,
        tasks: vec![
            PlannerTask {
                id: "main".to_string(),
                objective: "Modify main.py.".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: None,
                targets: vec!["main.py".to_string()],
                depends_on: vec![],
            },
            PlannerTask {
                id: "tests".to_string(),
                objective: "Add tests for main.py.".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: None,
                targets: vec!["tests/test_main.py".to_string()],
                depends_on: vec!["main".to_string()],
            },
        ],
    };
    assert!(
        validate_planner_tests_required(&output, &python_tests).is_ok(),
        "main.py plus tests/test_main.py must satisfy test-required planning"
    );
}

#[test]
fn tests_required_passes_when_adapter_requires_nothing() {
    // Invariant: when the adapter returns no required tests, any plan passes.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![PlannerTask {
            id: "main".to_string(),
            objective: "Modify main.py.".to_string(),
            name: String::new(),
            operation: Some(PlannerOperation::Modify),
            role: None,
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
fn plan_kind_with_no_tasks_fails_empty_task_list() {
    // Invariant: an empty `tasks` array fails validation regardless of
    // `kind` — `kind: "plan"` exempts target and role validation, but not
    // the non-empty `tasks` requirement.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![],
    };
    assert_eq!(
        validate_planner_output(&output),
        Err(PlannerValidationError::EmptyTaskList)
    );
}

// ── Mapping (Plan-node `PlannerOutput`) ─────────────────────────────────────

#[test]
fn planner_tasks_become_node_requests() {
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![
            PlannerTask {
                id: "step-one".to_string(),
                objective: "do step one".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: None,
                targets: vec!["one.txt".to_string()],
                depends_on: vec![],
            },
            PlannerTask {
                id: "step-two".to_string(),
                objective: "do step two".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: None,
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
fn task_kind_output_produces_no_scheduler_children() {
    // Invariant: `kind: "task"` has no corresponding scheduler `NodeKind`, so
    // `into_plan` must not panic and must not insert any scheduler nodes.
    // Recording the tasks into the manifest happens outside the scheduler
    // graph (`IntegrationService::integrate_planner_tasks`).
    let output = PlannerOutput {
        kind: PlannerOutputKind::Task,
        tasks: vec![PlannerTask {
            id: "sub-a".to_string(),
            objective: "decompose part a".to_string(),
            name: String::new(),
            operation: None,
            role: None,
            targets: vec![],
            depends_on: vec![],
        }],
    };
    let plan = planner_output_to_plan_output(output);
    assert!(plan.children.is_empty());
}

#[test]
fn task_kind_output_carries_name_into_plan_tasks() {
    // Invariant: `PlannerTask::name` is carried verbatim into
    // `PlannerTaskOutput::name`, alongside `id`/`objective`, so the manifest
    // recording path (`IntegrationService::integrate_plan_tasks`) has access
    // to it.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Task,
        tasks: vec![PlannerTask {
            id: "sub-a".to_string(),
            objective: "decompose part a".to_string(),
            name: "sub_a".to_string(),
            operation: None,
            role: None,
            targets: vec![],
            depends_on: vec![],
        }],
    };
    let plan = planner_output_to_plan_output(output);
    assert_eq!(plan.tasks[0].name, "sub_a");
}

#[test]
fn plan_kind_output_produces_plan_children_with_no_worker_role() {
    // Invariant: `kind: "plan"` maps every task to a `NodeKind::Plan` child,
    // and such children never get a "tester" worker role since they carry no
    // concrete targets.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![
            PlannerTask {
                id: "sub-a".to_string(),
                objective: "decompose part a".to_string(),
                name: String::new(),
                operation: None,
                role: None,
                targets: vec![],
                depends_on: vec![],
            },
            PlannerTask {
                id: "sub-b".to_string(),
                objective: "decompose part b".to_string(),
                name: String::new(),
                operation: None,
                role: None,
                targets: vec![],
                depends_on: vec!["sub-a".to_string()],
            },
        ],
    };
    let plan = planner_output_to_plan_output(output);
    assert_eq!(plan.children.len(), 2);
    for child in &plan.children {
        assert_eq!(child.kind, NodeKind::Plan);
        assert_eq!(child.worker_role, None);
    }
    assert_eq!(
        plan.children[1].dependencies,
        vec![NodeId("sub-a".to_string())]
    );
}

#[test]
fn task_role_becomes_node_request_worker_role() {
    // Invariant: the planner now assigns worker roles explicitly per task —
    // `task.role` is carried straight through to `NodeRequest::worker_role`,
    // with no framework classification based on targets.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![
            PlannerTask {
                id: "impl".to_string(),
                objective: "modify main.py".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: Some("implementer".to_string()),
                targets: vec!["main.py".to_string()],
                depends_on: vec![],
            },
            PlannerTask {
                id: "test".to_string(),
                objective: "add tests for main.py".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Create),
                role: Some("tester".to_string()),
                targets: vec!["tests/test_main.py".to_string()],
                depends_on: vec!["impl".to_string()],
            },
        ],
    };
    let plan = planner_output_to_plan_output(output);

    let impl_child = plan
        .children
        .iter()
        .find(|c| c.id == NodeId("impl".to_string()))
        .unwrap();
    assert_eq!(impl_child.worker_role, Some("implementer".to_string()));

    let test_child = plan
        .children
        .iter()
        .find(|c| c.id == NodeId("test".to_string()))
        .unwrap();
    assert_eq!(test_child.worker_role, Some("tester".to_string()));
}

#[test]
fn task_without_role_gets_no_worker_role() {
    // Invariant: a missing `role` means no worker role assigned — the
    // planner's absence of an assignment is not backfilled by the framework.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![PlannerTask {
            id: "combined".to_string(),
            objective: "modify main.py and its tests".to_string(),
            name: String::new(),
            operation: Some(PlannerOperation::Modify),
            role: None,
            targets: vec!["main.py".to_string(), "tests/test_main.py".to_string()],
            depends_on: vec![],
        }],
    };
    let plan = planner_output_to_plan_output(output);

    assert_eq!(plan.children[0].kind, NodeKind::Work);
    assert_eq!(plan.children[0].worker_role, None);
}

#[test]
fn planner_dependencies_preserved() {
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![
            PlannerTask {
                id: "tests".to_string(),
                objective: "write tests".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: None,
                targets: vec!["tests.txt".to_string()],
                depends_on: vec![],
            },
            PlannerTask {
                id: "impl".to_string(),
                objective: "implement".to_string(),
                name: String::new(),
                operation: Some(PlannerOperation::Modify),
                role: None,
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

#[test]
fn plan_children_inherit_parent_team_adapter_northstar() {
    // Invariant: a team-owned Plan node's recursive children inherit that
    // node's own `team`/`adapter`/`northstar` (as carried by the request that
    // produced this output), rather than the hardcoded empty single-team
    // default. Otherwise recursive planning loses which team/adapter/
    // northstar owns the newly spawned nodes.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Plan,
        tasks: vec![planner_task("sub-a", "decompose part a", &[], &[])],
    };
    let plan = processor(&no_required_test_targets).into_plan(
        output,
        "team-a".to_string(),
        "adapters/team-a.yaml".to_string(),
        "northstar/team-a.md".to_string(),
    );
    assert_eq!(plan.children[0].team, "team-a");
    assert_eq!(plan.children[0].adapter, "adapters/team-a.yaml");
    assert_eq!(plan.children[0].northstar, "northstar/team-a.md");
}

#[test]
fn root_plan_children_get_empty_team_adapter_northstar() {
    // Invariant: the single-team root/plan-expansion path carries empty
    // team/adapter/northstar into `into_plan`, and children inherit that same
    // emptiness — preserving today's behavior for runs with no team dispatch.
    let output = PlannerOutput {
        kind: PlannerOutputKind::Work,
        tasks: vec![planner_task("task", "do something", &["task.txt"], &[])],
    };
    let plan = planner_output_to_plan_output(output);
    assert_eq!(plan.children[0].team, "");
    assert_eq!(plan.children[0].adapter, "");
    assert_eq!(plan.children[0].northstar, "");
}
