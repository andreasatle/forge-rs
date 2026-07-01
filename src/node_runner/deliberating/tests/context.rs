use super::*;
use crate::machines::deliberation::DeliberationState;
use crate::node_runner::deliberating::request::prepare_deliberation;
use std::sync::Arc;

#[test]
fn prepared_deliberation_keeps_canonical_objective_and_structured_context_separate() {
    let temp = TempDir::new("structured-context-state");
    let view = make_artifact_view(&temp, "README.md", "This is the README.\n");
    let provider = ScriptedProvider::from_strs(&[]);
    let required_tests: Arc<TestTargetsFn> = Arc::new(|_| vec!["tests/test_main.py".to_string()]);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "do the thing".to_string(),
        target_files: vec!["src/main.py".to_string()],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };

    let prepared = prepare_deliberation(
        &provider,
        &request,
        1024,
        &RolePolicy::default(),
        &required_tests,
        &["README.md".to_string()],
    );

    let DeliberationState::Ready { request } = prepared.initial_state else {
        panic!("prepared deliberation must start in Ready state");
    };
    assert_eq!(request.objective, "do the thing");
    assert_eq!(
        request.context.target_files,
        vec!["src/main.py".to_string()]
    );
    assert!(request.context.testing_requirement.is_some());
    let artifact = request
        .context
        .artifact
        .expect("artifact context must be captured");
    assert_eq!(artifact.files, vec!["README.md".to_string()]);
    assert_eq!(artifact.selected_files[0].path, "README.md");
    assert_eq!(artifact.selected_files[0].content, "This is the README.\n");
}

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
        node_id: NodeId("test-node".to_string()),
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
        node_id: NodeId("test-node".to_string()),
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
        node_id: NodeId("test-node".to_string()),
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
        node_id: NodeId("test-node".to_string()),
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
