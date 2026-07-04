use std::sync::Arc;

use super::*;
use crate::validation::{ValidationPlan, ValidationStage, ValidationStep};

fn python_test_targets(targets: &[String]) -> Vec<String> {
    let rules = crate::language::language_spec("python")
        .expect("python language spec must load")
        .validation
        .validation_targets;
    crate::validation::derive_validation_targets(&rules, targets)
}

fn marker_plan(marker: &str) -> ValidationPlan {
    ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["echo".to_string(), marker.to_string()],
            when_artifacts_present: vec![],
            scope: crate::validation::ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 120,
    }
}

#[test]
fn plan_stamps_work_and_validation_children_with_distinct_plans() {
    // Invariant: a planner task whose targets are entirely derived validation
    // targets (e.g. a test file) is assigned the "tester" worker role and
    // stamped with the runner's validation_node_plan, while a task targeting
    // source files gets no worker role and the work_node_plan — the two
    // roles must never share the same validation contract when distinct
    // plans are configured.
    let plan_json = r#"{"tasks":[
        {"id":"task-1","objective":"Modify main.py","operation":"modify","targets":["main.py"],"depends_on":[]},
        {"id":"task-2","objective":"Add tests for main.py","operation":"create","targets":["tests/test_main.py"],"depends_on":["task-1"]}
    ]}"#;
    let provider = ScriptedProvider::from_strs(&[
        plan_json,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_required_test_targets_fn(Arc::new(python_test_targets))
        .with_work_node_plan(Some(marker_plan("work-marker")))
        .with_validation_node_plan(Some(marker_plan("validation-marker")));

    let result = runner.run_node(plan_request("Modify main.py and its tests"), &NoopTelemetry);
    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted");
    };

    let work_child = plan
        .children
        .iter()
        .find(|c| c.target_files == vec!["main.py".to_string()])
        .expect("main.py task must be present");
    assert_eq!(work_child.kind, NodeKind::Work);
    assert_eq!(work_child.worker_role, None);
    assert_eq!(work_child.validation_plan, Some(marker_plan("work-marker")));
    assert_eq!(
        work_child.required_validation_targets,
        vec!["tests/test_main.py".to_string()],
        "Work node must carry its derived required validation target"
    );

    let validation_child = plan
        .children
        .iter()
        .find(|c| c.target_files == vec!["tests/test_main.py".to_string()])
        .expect("tests/test_main.py task must be present");
    assert_eq!(validation_child.kind, NodeKind::Work);
    assert_eq!(validation_child.worker_role, Some("tester".to_string()));
    assert_eq!(
        validation_child.validation_plan,
        Some(marker_plan("validation-marker"))
    );
    assert!(
        validation_child.required_validation_targets.is_empty(),
        "tester node must not carry its own required validation targets"
    );
}

#[test]
fn deliberating_runner_threads_max_tokens_to_provider() {
    let provider = CapturingProvider::from_strs(&[
        r#"{"summary":"task completed"}"#,
        r#"{"status":"accepted","content":"review done"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_cheap_max_tokens(256);
    runner.run_node(work_request("test threading"), &NoopTelemetry);

    assert_eq!(
        provider.captured_max_tokens(),
        Some(256),
        "with_cheap_max_tokens must propagate to the provider request"
    );
}

#[test]
fn runtime_uses_project_adapter_role_policy() {
    use crate::project::DefaultProjectAdapter;
    use crate::project::ProjectAdapter;

    // Simulate the runtime: get policy from adapter, wire into runner.
    let adapter = DefaultProjectAdapter;
    let policy = adapter.role_policy();

    // A custom marker in a policy derived from the adapter should reach the prompt.
    let custom_policy = crate::roles::RolePolicy {
        worker_producer_system: "ADAPTER_MARKER_TEST".to_string(),
        ..policy
    };

    let provider = RecordingProvider::from_strs(&[
        r#"{"summary":"completed"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_role_policy(custom_policy);
    runner.run_node(work_request("test policy wiring"), &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    assert!(
        prompts[0].contains("ADAPTER_MARKER_TEST"),
        "adapter role policy must reach the provider prompt; got:\n{}",
        prompts[0]
    );
}

// --- model-tier routing tests ---

#[test]
fn cheap_tier_uses_cheap_provider() {
    // Strong has no responses; calling it would panic. Proves routing is correct.
    let temp = TempDir::new("cheap-tier");
    let cheap = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"task completed\n"}"#,
        r#"{"summary":"task completed"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let strong = ScriptedProvider::from_strs(&[]);
    let runner = DeliberatingNodeRunner::new(&cheap, &strong);
    let result = runner.run_node(
        work_request_with_artifact("cheap tier test", &temp),
        &NoopTelemetry,
    );
    assert!(
        matches!(result, NodeRunResult::WorkAccepted(_)),
        "cheap tier must route to cheap provider and succeed"
    );
}

#[test]
fn strong_tier_uses_strong_provider() {
    // Cheap has no responses; calling it would panic. Proves routing is correct.
    let temp = TempDir::new("strong-tier");
    let cheap = ScriptedProvider::from_strs(&[]);
    let strong = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"task completed\n"}"#,
        r#"{"summary":"task completed"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&cheap, &strong);
    let result = runner.run_node(
        strong_work_request_with_artifact("strong tier test", &temp),
        &NoopTelemetry,
    );
    assert!(
        matches!(result, NodeRunResult::WorkAccepted(_)),
        "strong tier must route to strong provider and succeed"
    );
}

#[test]
fn strong_tier_uses_strong_token_budget() {
    // Cheap has no responses — if it were called the test would panic.
    let cheap = CapturingProvider::from_strs(&[]);
    let strong = CapturingProvider::from_strs(&[
        r#"{"summary":"task completed"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&cheap, &strong)
        .with_cheap_max_tokens(512)
        .with_strong_max_tokens(2048);
    runner.run_node(strong_work_request("token budget test"), &NoopTelemetry);

    assert_eq!(
        strong.captured_max_tokens(),
        Some(2048),
        "strong tier must use strong_max_tokens"
    );
    assert_eq!(
        cheap.captured_max_tokens(),
        None,
        "cheap provider must not be called for a strong-tier request"
    );
}
