use std::sync::Arc;

use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

use super::*;

#[test]
fn fast_plan_bypasses_provider_and_emits_telemetry() {
    // When the objective names exactly one source file the fast path must
    // produce PlanAccepted without calling the provider at all.
    // ScriptedProvider panics when exhausted — no responses means any call is a bug.
    let provider = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        NodeRunRequest {
            kind: NodeKind::Plan,
            node_id: NodeId("test-node".to_string()),
            objective: "Create a simple Python program in main.py that prints a greeting."
                .to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            work_attempt: None,
        },
        &telemetry,
    );

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted from fast path; got another variant");
    };
    assert_eq!(plan.children.len(), 1, "no tests required → one work task");
    assert!(
        plan.children[0].target_files == vec!["main.py".to_string()],
        "fast plan work task must target main.py"
    );

    let records = telemetry.into_records();
    let fast_plan_event = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FastPlanUsed { .. }
        )
    });
    assert!(
        fast_plan_event.is_some(),
        "fast path must emit FastPlanUsed telemetry"
    );
    if let Some(r) = fast_plan_event {
        match &r.event {
            crate::telemetry::TelemetryEvent::FastPlanUsed { task_count } => {
                assert_eq!(*task_count, 1);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn fast_plan_with_tests_required_adds_test_task_and_emits_telemetry() {
    let provider = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_required_test_targets_fn(Arc::new(|t| CodingProjectAdapter.required_test_targets(t)));
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        NodeRunRequest {
            kind: NodeKind::Plan,
            node_id: NodeId("test-node".to_string()),
            objective: "Create a simple Python program in main.py that prints a greeting."
                .to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            work_attempt: None,
        },
        &telemetry,
    );

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted from fast path");
    };
    assert_eq!(plan.children.len(), 2, "tests required → two work tasks");
    assert!(
        plan.children
            .iter()
            .any(|c| c.target_files == vec!["main.py".to_string()]),
        "must have a main.py work task"
    );
    assert!(
        plan.children
            .iter()
            .any(|c| c.target_files == vec!["test_main.py".to_string()]),
        "must have a test_main.py task"
    );

    let records = telemetry.into_records();
    let fast_plan_event = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FastPlanUsed { .. }
        )
    });
    assert!(
        fast_plan_event.is_some(),
        "fast path with tests must emit FastPlanUsed telemetry"
    );
    if let Some(r) = fast_plan_event {
        match &r.event {
            crate::telemetry::TelemetryEvent::FastPlanUsed { task_count } => {
                assert_eq!(*task_count, 2);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn multi_file_objective_falls_through_to_llm_planner() {
    // Two explicit source files → fast path returns None → LLM planner called.
    // Targets must be within the allowed set (main.py, utils.py) to pass validation.
    let tasks_json = r#"{"tasks":[{"id":"w","objective":"add logging","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        NodeRunRequest {
            kind: NodeKind::Plan,
            node_id: NodeId("test-node".to_string()),
            objective: "Modify main.py and utils.py to add logging.".to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            work_attempt: None,
        },
        &NoopTelemetry,
    );
    assert!(
        matches!(result, NodeRunResult::PlanAccepted(_)),
        "multi-file objective must fall through to the LLM planner and succeed"
    );
}
