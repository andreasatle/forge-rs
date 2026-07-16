use super::*;
use crate::language::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};
use crate::machines::deliberation::DeliberationState;
use crate::node_runner::deliberating::context::DeliberationContextConfig;
use crate::node_runner::deliberating::request::prepare_deliberation;
use crate::validation::{CommandSpec, ValidationScope};
use std::collections::BTreeMap;
use std::sync::Arc;

fn cat_command() -> CommandSpec {
    CommandSpec {
        program: "cat".to_string(),
        args: vec![],
        when_files_present: vec![],
        scope: ValidationScope::Workspace,
    }
}

/// A minimal language plugin with distinctive prompt guidance, for one
/// extension.
fn plugin_spec(extension: &str, instructions: &str) -> LanguageSpec {
    LanguageSpec {
        extensions: vec![extension.to_string()],
        identity: String::new(),
        context: String::new(),
        instructions: instructions.to_string(),
        constraints: String::new(),
        init: LanguageInitSpec {
            gitignore: vec![],
            commands: vec![],
        },
        validation: LanguageValidationSpec {
            runs_tests: false,
            commands: vec![],
            validation_targets: vec![],
        },
        plugin_roles: vec![],
        api_summary: None,
        name_target_rules: vec![],
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    };

    let language_plugins = BTreeMap::new();
    let context_config = DeliberationContextConfig {
        required_test_targets_fn: &required_tests,
        context_file_names: &["README.md".to_string()],
        api_summary_command: None,
        northstar: None,
        language_plugins: &language_plugins,
        active_language_plugin: None,
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
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
    // Invariant: when the language plugin configures api_summary, Plan node
    // prompts must include a "Current artifact state" section built from
    // that command's per-file output.
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("## Current Artifact State"),
        "plan prompt must include the artifact state section; got:\n{first}"
    );
    assert!(
        first.contains("# main.py\ndef f():\n    pass"),
        "plan prompt must include the per-file api summary output; got:\n{first}"
    );
}

#[test]
fn northstar_section_appears_in_plan_node_prompt_when_configured() {
    // Invariant: when a northstar is configured, Plan node prompts must
    // surface it as a "## Northstar" section alongside the API summary so the
    // producer can plan the gap between the two.
    let temp = TempDir::new("northstar-plan");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let plan = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose the CLI work","name":"cli_work","depends_on":[]}]}"#;
    let provider = RecordingProvider::from_strs(&[
        plan,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_api_summary_command(Some(cat_command()))
        .with_northstar(Some("Ship a fibonacci CLI.".to_string()));
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        node_id: NodeId("test-node".to_string()),
        objective: "Ship a fibonacci CLI.".to_string(),
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
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("## Northstar\nShip a fibonacci CLI."),
        "plan prompt must include the northstar section; got:\n{first}"
    );
}

#[test]
fn northstar_section_is_absent_for_work_nodes_even_when_configured() {
    // Invariant: the northstar section is a planning-time gap-analysis aid;
    // Work node prompts must never include it.
    let temp = TempDir::new("northstar-work");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"summary":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_northstar(Some("Ship a fibonacci CLI.".to_string()));
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "Add a function to main.py".to_string(),
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
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        !first.contains("## Northstar"),
        "work node prompt must not include the northstar section; got:\n{first}"
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
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    };
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        !first.contains("## Current Artifact State"),
        "work node prompt must not include the artifact state section; got:\n{first}"
    );
}

#[test]
fn language_plugin_matching_node_target_extension_appears_in_prompt() {
    // Invariant: the language plugin whose extension matches this node's own
    // target files is selected per node and its prompt sections appear in
    // the rendered prompt — not baked into every node regardless of language.
    let temp = TempDir::new("plugin-prompt-match");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"summary":"approved"}"#,
    ]);
    let mut plugins = BTreeMap::new();
    plugins.insert(
        "py".to_string(),
        plugin_spec("py", "Follow PEP 8 conventions."),
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_language_plugins(plugins);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "do the thing".to_string(),
        target_files: vec!["main.py".to_string()],
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
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        first.contains("Follow PEP 8 conventions."),
        "prompt for a .py target must include the matching plugin's guidance; got:\n{first}"
    );
}

#[test]
fn language_plugin_is_absent_from_plan_node_prompt_even_with_target_files() {
    // Invariant: a split node re-plans a failed node and inherits its
    // `target_files` for objective-rendering context (see
    // `recovery::apply_split`), but a Plan node never produces
    // language-specific code itself — that inherited list must not select a
    // language plugin into its prompt.
    let temp = TempDir::new("plugin-prompt-plan");
    let view = make_artifact_view(&temp, "main.py", "def f():\n    pass\n");

    let plan = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"re-plan main.py","name":"main","depends_on":[]}]}"#;
    let provider = RecordingProvider::from_strs(&[
        plan,
        r#"{"status":"accepted","content":"plan looks good"}"#,
        r#"{"status":"accepted","content":"plan approved"}"#,
    ]);
    let mut plugins = BTreeMap::new();
    plugins.insert(
        "py".to_string(),
        plugin_spec("py", "Follow PEP 8 conventions."),
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_language_plugins(plugins);
    let request = NodeRunRequest {
        kind: NodeKind::Plan,
        node_id: NodeId("test-node".to_string()),
        objective: "Fix main.py".to_string(),
        target_files: vec!["main.py".to_string()],
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
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        !first.contains("Follow PEP 8 conventions."),
        "plan prompt must not include plugin guidance even when target_files is non-empty; got:\n{first}"
    );
}

#[test]
fn language_plugin_not_matching_node_target_extension_is_absent_from_prompt() {
    // Invariant: a declared plugin whose extension does not match this
    // node's target files must not leak into its prompt — each node only
    // ever sees the plugin selected for its own language, never every
    // declared plugin.
    let temp = TempDir::new("plugin-prompt-mismatch");
    let view = make_artifact_view(&temp, "main.rs", "fn f() {}\n");

    let provider = RecordingProvider::from_strs(&[
        r#"{"summary":"draft output"}"#,
        r#"{"tool":"read_file","path":"main.rs"}"#,
        r#"{"summary":"review ok"}"#,
        r#"{"tool":"read_file","path":"main.rs"}"#,
        r#"{"summary":"approved"}"#,
    ]);
    let mut plugins = BTreeMap::new();
    plugins.insert(
        "py".to_string(),
        plugin_spec("py", "Follow PEP 8 conventions."),
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider).with_language_plugins(plugins);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "do the thing".to_string(),
        target_files: vec!["main.rs".to_string()],
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
    runner.run_node(request, &NoopTelemetry);

    let prompts = provider.recorded_prompts();
    assert!(!prompts.is_empty(), "provider must have received prompts");
    let first = &prompts[0];
    assert!(
        !first.contains("Follow PEP 8 conventions."),
        "prompt for a .rs target must not include the python plugin's guidance; got:\n{first}"
    );
}
