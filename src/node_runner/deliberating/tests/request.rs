use super::*;

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
