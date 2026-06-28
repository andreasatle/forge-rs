use super::*;

#[test]
fn artifact_worker_without_tool_update_fails_semantic_validation() {
    let temp = TempDir::new("artifact-work-missing-update");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"summary of the work done"}"#,
        r#"{"status":"accepted","content":"summary of the work done"}"#,
        r#"{"status":"accepted","content":"summary of the work done"}"#,
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

    // Round 1: Producer claims done, Critic reads and accepts, Referee reads
    // main.py, sees default content, and rejects.
    // Round 2: same sequence; budget is now exhausted → node fails.
    let provider = ScriptedProvider::from_strs(&[
        // Round 1
        r#"{"status":"accepted","content":"I wrote the haiku"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"file is present"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
        // Round 2
        r#"{"status":"accepted","content":"I wrote the haiku"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"file is present"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"rejected","reason":"main.py still has default init content, not a haiku"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "Write a haiku about Python state machines in main.py".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
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
        r#"{"status":"accepted","content":"I read the file and it looks correct"}"#,
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
        objective: "write the work".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "node must fail when Critic never reads, even though Producer did"
    );
}

// ── no-recreate validation recovery regression ────────────────────────────

#[test]
fn planner_no_recreate_violation_sends_revision_feedback_and_retries() {
    // Regression: planner first outputs a task for '.gitignore' (existing project
    // file not mentioned in the objective). Validation rejects it and sends structured
    // feedback. Planner revises to only include the main.py task. Run continues to
    // PlanAccepted — the run must NOT terminate with a terminal failure.
    let temp = TempDir::new("no-recreate-retry");
    let view = make_artifact_view(&temp, ".gitignore", "*.pyc\n__pycache__/\n");

    // First planner response: includes .gitignore task (violates no-recreate).
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Create .gitignore file for the project.","operation":"modify","targets":[".gitignore"],"depends_on":[]},{"id":"task-2","objective":"Write main.py with the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    // Second planner response (after revision feedback): only the main.py task.
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Write main.py with the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails no-recreate, handler retries
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called with the scripted responses.
        objective: "Write a haiku about Python state machines.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner revision");
    };
    assert_eq!(
        plan.children.len(),
        1,
        "revised plan must contain only the main.py task"
    );
    assert_eq!(plan.children[0].objective, "Write main.py with the haiku.");
    assert_eq!(plan.children[0].target_files, vec!["main.py".to_string()]);
}

#[test]
fn planner_missing_test_target_sends_revision_feedback_and_retries() {
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"modify","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails test-target validation
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called with the scripted responses.
        objective: "Print a short haiku about state machines.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner adds test target");
    };
    assert_eq!(plan.children.len(), 2);
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["test_main.py".to_string()]),
        "revised plan must include a test_main.py target"
    );
}

#[test]
fn planner_explicit_target_violation_sends_revision_feedback_and_retries() {
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Modify project configuration.","operation":"modify","targets":["pyproject.toml"],"depends_on":[]},{"id":"task-3","objective":"Add tests for the main.py haiku behavior.","operation":"create","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;
    let good_plan = r#"{"tasks":[{"id":"task-1","objective":"Modify main.py to return the haiku.","operation":"modify","targets":["main.py"],"depends_on":[]},{"id":"task-2","objective":"Add tests for the main.py haiku behavior.","operation":"create","targets":["test_main.py"],"depends_on":["task-1"]}]}"#;

    let provider = RecordingProvider::from_strs(&[
        bad_plan,  // Plan+Producer attempt 1 — fails explicit-target validation
        good_plan, // Plan+Producer attempt 2 (with feedback) — passes
        r#"{"status":"accepted","content":"plan looks good"}"#, // Plan+Critic
        r#"{"status":"accepted","content":"plan approved"}"#, // Plan+Referee
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_requires_tests(true);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Two explicit files → fast path does not apply (needs exactly one source file);
        // ExplicitTargetViolation still fires when the planner targets pyproject.toml.
        objective: "Modify main.py and utils.py to print a short haiku.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted after planner removes pyproject.toml");
    };
    assert_eq!(plan.children.len(), 2);
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["main.py".to_string()]),
        "revised plan must include main.py"
    );
    assert!(
        plan.children
            .iter()
            .any(|child| child.target_files == vec!["test_main.py".to_string()]),
        "revised plan must include test_main.py"
    );
    assert!(
        plan.children
            .iter()
            .all(|child| !child.objective.contains("pyproject.toml")),
        "revised plan must reject pyproject.toml"
    );

    let prompts = provider.recorded_prompts();
    assert!(
        prompts.iter().any(|prompt| prompt.contains(
            "The objective explicitly targets main.py, utils.py. \
                 Remove all non-test targets except main.py, utils.py."
        )),
        "retry prompt must contain exact explicit-target feedback; got: {prompts:#?}"
    );
}

#[test]
fn planner_no_recreate_violation_exhausts_retries_returns_failed() {
    // When the planner keeps including tasks for existing files after MAX retries,
    // the run must fail — not silently accept the bad plan.
    let temp = TempDir::new("no-recreate-exhausted");
    let view = make_artifact_view(&temp, ".gitignore", "*.pyc\n");

    // All three producer responses include the .gitignore task.
    let bad_plan = r#"{"tasks":[{"id":"task-1","objective":"Create .gitignore file.","operation":"modify","targets":[".gitignore"],"depends_on":[]}]}"#;

    let provider = ScriptedProvider::from_strs(&[
        bad_plan, // attempt 1
        bad_plan, // attempt 2 (retry 1)
        bad_plan, // attempt 3 (retry 2 — MAX_NO_RECREATE_RETRIES)
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        // Objective does not name a specific file so the fast path does not apply
        // and the LLM planner is called until retries are exhausted.
        objective: "Write a haiku about Python state machines.".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
    };
    let result = runner.run_node(request, &NoopTelemetry);

    assert!(
        matches!(result, NodeRunResult::Failed(_)),
        "plan must fail when no-recreate retries are exhausted"
    );
    if let NodeRunResult::Failed(failure) = result {
        assert!(
            failure.message.contains(".gitignore") || failure.message.contains("no-recreate"),
            "failure reason must mention the offending file or constraint; got: {}",
            failure.message
        );
    }
}
