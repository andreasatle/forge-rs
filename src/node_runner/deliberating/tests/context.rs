use super::*;

#[test]
fn artifact_view_context_is_visible_to_deliberation_prompt() {
    let temp = TempDir::new("prompt-context");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    // Critic and Referee are Work reviewers and must call read_file before
    // accepting.  Add read_file("hello.txt") calls for each reviewer.
    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do the thing".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("hello.txt"),
        "first prompt must list artifact files; got:\n{first}"
    );
    assert!(
        first.contains("do the thing"),
        "first prompt must include the original objective; got:\n{first}"
    );
}

#[test]
fn context_file_content_is_included_in_prompt_when_present() {
    let temp = TempDir::new("context-file-prompt");
    let view = make_artifact_view(&temp, "README.md", "This is the README.\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"README.md"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"README.md"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_context_file_names(vec!["README.md".to_string()]);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do the thing".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("This is the README."),
        "first prompt must include the README.md content; got:\n{first}"
    );
    assert!(
        first.contains("README.md"),
        "first prompt must name the context file; got:\n{first}"
    );
}

#[test]
fn absent_context_file_is_silently_omitted_from_prompt() {
    let temp = TempDir::new("context-file-absent");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    // Ask for README.md which does not exist in this artifact.
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_context_file_names(vec!["README.md".to_string()]);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do something".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("hello.txt"),
        "first prompt must still list artifact files; got:\n{first}"
    );
}

#[test]
fn no_context_file_names_produces_no_extra_content() {
    let temp = TempDir::new("no-context-files");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"status":"accepted","content":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "do something".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    let first = &prompts[0];
    assert!(
        first.contains("hello.txt"),
        "first prompt must list artifact files; got:\n{first}"
    );
    assert!(
        !first.contains("README.md"),
        "first prompt must not mention README.md when no context files configured; got:\n{first}"
    );
}
