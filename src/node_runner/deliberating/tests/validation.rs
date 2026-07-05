use std::sync::Arc;

use super::*;

fn python_test_targets(targets: &[String]) -> Vec<String> {
    let rules = crate::language::language_spec("python")
        .expect("python language spec must load")
        .validation
        .validation_targets;
    crate::validation::derive_validation_targets(&rules, targets)
}

#[test]
fn artifact_worker_without_tool_update_fails_semantic_validation() {
    let temp = TempDir::new("artifact-work-missing-update");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"summary":"summary of the work done"}"#,
        r#"{"summary":"summary of the work done"}"#,
        r#"{"summary":"summary of the work done"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        work_request_with_artifact("do some work", &temp),
        &NoopTelemetry,
    );
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(matches!(
        failure.kind,
        FailureKind::WorkSemanticValidationFailure
    ));
    assert!(
        matches!(failure.recovery, RecoveryAction::Retry { .. }),
        "WorkSemanticValidationFailure must be retryable; got {:?}",
        failure.recovery
    );
}

#[test]
fn referee_reads_file_and_rejects_default_content_causes_node_failure() {
    // Regression for: Referee accepted even though main.py still contained the
    // default initialized program instead of the required haiku.
    //
    // The Referee must call read_file and inspect file contents before deciding.
    // When the file contents do not satisfy the objective the Referee must reject.
    // Two rounds of rejection (max_revisions = 1) exhaust the revision budget
    // and the node must fail — WorkAccepted must never be returned.
    let temp = TempDir::new("referee-default-content");
    let view = make_artifact_view(&temp, "main.py", r#"print("Hello from forge-lang-init!")"#);
    let work_attempt = work_attempt_for_view(&view);

    // Round 1: Producer writes (still-default) content and claims done, Critic
    // reads and accepts, Referee reads main.py, sees default content, and rejects.
    // Round 2: same sequence; budget is now exhausted → node fails.
    let provider = ScriptedProvider::from_strs(&[
        // Round 1
        r#"{"tool":"write_file","path":"main.py","content":"print(\"Hello from forge-lang-init!\")"}"#,
        r#"{"summary":"I wrote the haiku"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"file is present"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
        // Round 2
        r#"{"tool":"write_file","path":"main.py","content":"print(\"Hello from forge-lang-init!\")"}"#,
        r#"{"summary":"I wrote the haiku"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"file is present"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "Write a haiku about Python state machines in main.py".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: Some(work_attempt),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "node must fail when Referee rejects incorrect file contents"
    );
}

#[test]
fn producer_read_file_does_not_satisfy_critic_read_requirement() {
    // The read_file_executed flag is scoped per role invocation.
    // Even if the Producer successfully read a file, the Critic's own
    // invocation starts fresh with read_file_executed = false.
    // A Critic that never reads must fail the enforcement regardless of
    // what the Producer did.
    let temp = TempDir::new("producer-read-no-critic");
    let view = make_artifact_view(&temp, "hello.txt", "hello world\n");

    // Producer: reads hello.txt (success), then accepts.
    // Critic: accepts three times without reading, exhausting protocol retries.
    // The deliberation must fail — not succeed because Producer already read.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"I read the file and it looks correct"}"#,
        // Critic attempt 1: enforcement fires, must-read retry issued
        r#"{"status":"accepted","content":"looks good to me here ok"}"#,
        // Critic attempt 2: enforcement fires again
        r#"{"status":"accepted","content":"still looks good to me now"}"#,
        // Critic attempt 3: enforcement fires, protocol_attempt > MAX_PROTOCOL_RETRIES → fail
        r#"{"status":"accepted","content":"I accept this work done now"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "write the work".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "node must fail when Critic never reads, even though Producer did"
    );
}

#[test]
fn planner_missing_test_target_sends_revision_feedback_and_retries() {
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"modify","targets":["tests/test_main.py"],"depends_on":["task-1"]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails test-target validation
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_required_test_targets_fn(Arc::new(python_test_targets));
    let request = NodeRunRequest {
        kind: NodeKind::OldPlan,
        node_id: NodeId("test-node".to_string()),
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called with the scripted responses.
        objective: "Print a short haiku about state machines.".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
        worker_role: None,
        work_attempt: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner adds test target");
    };
    assert_eq!(plan.children.len(), 2);
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["tests/test_main.py".to_string()]),
        "revised plan must include a tests/test_main.py target"
    );
}
