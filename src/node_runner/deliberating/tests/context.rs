use super::*;
use crate::machines::deliberation::DeliberationState;
use crate::node_runner::deliberating::context::DeliberationContextConfig;
use crate::node_runner::deliberating::request::prepare_deliberation;
use crate::validation::{CommandSpec, ValidationScope};
use std::sync::Arc;

fn cat_command() -> CommandSpec {
    CommandSpec {
        program: "cat".to_string(),
        args: vec![],
        when_files_present: vec![],
        scope: ValidationScope::Workspace,
    }
}

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
        worker_role: None,
        work_attempt: None,
    };

    let context_config = DeliberationContextConfig {
        required_test_targets_fn: &required_tests,
        context_file_names: &["README.md".to_string()],
        api_summary_command: None,
        northstar: None,
    };
    let prepared = prepare_deliberation(
        &provider,
        &request,
        1024,
        &RolePolicy::default(),
        &context_config,
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
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"approved"}"#,
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
        worker_role: None,
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
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"README.md"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"README.md"}"#,
        r#"{"summary":"approved"}"#,
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
        worker_role: None,
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
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"approved"}"#,
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
        worker_role: None,
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
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"approved"}"#,
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
        worker_role: None,
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

#[test]
fn api_summary_section_appears_in_plan_node_prompt_when_configured() {
    // Invariant: when the language plugin configures api_summary, Decomposition
    // and Plan node prompts must include a "Current artifact state" section
    // built from that command's per-file output.
    let temp = TempDir::new("api-summary-plan");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let plan = r#"{"tasks":[{"id":"task-1","objective":"Add a function.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let provider = RecordingProvider::from_strs(&[
        plan,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_api_summary_command(Some(cat_command()));
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        node_id: NodeId("test-node".to_string()),
        objective: "Add a function to main.py".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("Current artifact state:"),
        "plan prompt must include the artifact state section; got:\n{first}"
    );
    assert!(
        first.contains("# main.py\ndef f():\n    pass"),
        "plan prompt must include the per-file api summary output; got:\n{first}"
    );
}

#[test]
fn northstar_section_appears_in_decomposition_node_prompt_when_configured() {
    // Invariant: when a northstar is configured, Decomposition node prompts
    // must surface it as a "Northstar:" section alongside the API summary so
    // the producer can plan the gap between the two.
    let temp = TempDir::new("northstar-decomposition");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let decomposition = r#"{"kind":"plan"}"#;
    let provider = RecordingProvider::from_strs(&[
        decomposition,
        r#"{"status":"accepted","content":"decomposition looks good"}"#,
        r#"{"status":"accepted","content":"decomposition approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_api_summary_command(Some(cat_command()))
        .with_northstar(Some("Ship a fibonacci CLI.".to_string()));
    let request = NodeRunRequest {
        kind: NodeKind::Decomposition,
        node_id: NodeId("test-node".to_string()),
        objective: "Ship a fibonacci CLI.".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("Northstar:\nShip a fibonacci CLI."),
        "decomposition prompt must include the northstar section; got:\n{first}"
    );
}

#[test]
fn northstar_section_is_absent_for_plan_nodes_even_when_configured() {
    // Invariant: the northstar section is a Decomposition-only gap-analysis
    // aid; Plan node prompts must never include it.
    let temp = TempDir::new("northstar-plan");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let plan = r#"{"tasks":[{"id":"task-1","objective":"Add a function.","operation":"modify","targets":["main.py"],"depends_on":[]}]}"#;
    let provider = RecordingProvider::from_strs(&[
        plan,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_northstar(Some("Ship a fibonacci CLI.".to_string()));
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        node_id: NodeId("test-node".to_string()),
        objective: "Add a function to main.py".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        !first.contains("Northstar:"),
        "plan node prompt must not include the northstar section; got:\n{first}"
    );
}

#[test]
fn api_summary_section_is_absent_for_work_nodes_even_when_configured() {
    // Invariant: api_summary is a planning-time aid — Work node prompts must
    // never include it, even when the language plugin configures the command.
    let temp = TempDir::new("api-summary-work");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"summary":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_api_summary_command(Some(cat_command()));
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "do the thing".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: None,
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        !first.contains("Current artifact state:"),
        "work node prompt must not include the artifact state section; got:\n{first}"
    );
}
