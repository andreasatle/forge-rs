use super::*;

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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "node must fail when Critic never reads, even though Producer did"
    );
}
