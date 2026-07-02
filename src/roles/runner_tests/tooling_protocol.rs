use super::*;

#[test]
fn placeholder_tool_echo_produces_informative_error_in_retry_prompt() {
    // The model echoes the write_file example verbatim with $TARGET_FILE /
    // $FILE_CONTENT. The retry prompt must mention "placeholder" so the model
    // understands WHY its response was rejected, not just that it was invalid.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"$TARGET_FILE","content":"$FILE_CONTENT"}"#,
        r#"{"summary":"wrote the actual file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must recover on retry; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let retry_prompt = &requests[1].prompt;
    assert!(
        retry_prompt.contains("placeholder"),
        "retry prompt must mention 'placeholder' so the model knows why it was rejected; got:\n{retry_prompt}"
    );
}

#[test]
fn malformed_tool_call_reports_tool_error_not_role_parse_error() {
    // A tool call with a recognized "tool" field but a missing required
    // field (here: read_file with no "path") must surface the tool-call
    // parse error in the retry prompt, not the unrelated role-response
    // parse error (e.g. "missing field `summary`") that would otherwise
    // tell the model to fix the wrong thing.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file"}"#,
        r#"{"summary":"read the file after fixing the call"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("read a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must recover on retry; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let retry_prompt = &requests[1].prompt;
    assert!(
        retry_prompt.contains("malformed tool call"),
        "retry prompt must attribute the failure to the tool call; got:\n{retry_prompt}"
    );
    assert!(
        !retry_prompt.contains("missing field `summary`"),
        "retry prompt must not blame the role-response schema for a tool-call failure; got:\n{retry_prompt}"
    );
}

#[test]
fn protocol_failure_after_write_reason_is_prefixed() {
    // Producer calls write_file successfully (completion pressure active), then
    // exhausts all protocol retries returning bad JSON. The terminal failure
    // reason must start with "protocol failure after write:" so the classifier
    // can treat it as Retry rather than Terminal.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        "not json at all",
        "also not json",
        "still not json",
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write and confirm")),
        &crate::telemetry::NoopTelemetry,
    );

    let reason = match &output.result {
        RoleResult::Failed { reason, .. } => reason.clone(),
        other => panic!("expected Failed; got {other:?}"),
    };
    assert!(
        reason.starts_with("protocol failure after write:"),
        "terminal reason must start with 'protocol failure after write:'; got: {reason}"
    );
    assert_eq!(
        provider.requests.borrow().len(),
        4,
        "write_file + 3 failed final-response attempts"
    );
}

#[test]
fn echoed_tool_placeholder_triggers_parse_failure_not_tool_execution() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // A confused model sometimes echoes the tool-section examples verbatim.
    // These must be treated as parse failures and trigger a retry, NOT executed.
    for (case, placeholder_response, second_response, expected_content) in [
        (
            "replace_text",
            r#"{"tool":"replace_text","path":"output.txt","old":"...","new":"..."}"#,
            r#"{"summary":"haiku written"}"#,
            "haiku written",
        ),
        (
            "write_file",
            r#"{"tool":"write_file","path":"output.txt","content":"..."}"#,
            r#"{"summary":"completed"}"#,
            "completed",
        ),
    ] {
        let provider = ScriptedProvider::from_strs(&[placeholder_response, second_response]);
        let runner = ProviderRoleRunner::new(&provider);
        let telemetry = VecTelemetry::new();

        let output = runner.run_role(
            make_role_request(DeliberationRole::Producer, "write a file"),
            &telemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == expected_content),
            "[{case}] placeholder tool must not execute; got {:?}",
            output.result
        );
        let records = telemetry.records();
        assert!(
            records
                .iter()
                .all(|r| !matches!(r.event, TelemetryEvent::ToolRequested { .. })),
            "[{case}] placeholder tool must not emit ToolRequested"
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(&r.event, TelemetryEvent::ParseFailed { .. })),
            "[{case}] placeholder tool must emit ParseFailed"
        );
    }
}

// ── prompt hardening: no "..." placeholders in any rendered prompt ───────

#[test]
fn planner_tool_request_produces_error_observation() {
    // Even if a plan-node model emits a tool request, it gets "no file tools available"
    // rather than actual execution, because tool_context is None for plan nodes.
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider =
        ScriptedProvider::from_strs(&[r#"{"tool":"read_file","path":"hello.txt"}"#, tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "plan node must accept valid PlannerOutput after tool error; got {:?}",
        output.result
    );
    let second_prompt = &provider.requests.borrow()[1].prompt;
    assert!(
        second_prompt.contains("no file tools available"),
        "plan tool request must produce error observation; got:\n{second_prompt}"
    );
}

#[test]
fn tool_observation_warns_not_to_copy_observation_json() {
    let (_temp, view) = make_view("obs-warn");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_tool_context(producer_request("read hello.txt"), view),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("Framework tool observation:"),
        "observation section must use 'Framework tool observation:' header; got:\n{second_prompt}"
    );
    assert!(
        second_prompt.contains("not a valid response format"),
        "observation section must warn model not to copy it; got:\n{second_prompt}"
    );
}

#[test]
fn observation_json_echo_triggers_protocol_retry_not_tool_execution() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // Sequence: write_file (recorded), then model echoes the observation
    // JSON {"ok":true,"description":"write out.txt"} as its response,
    // then model finally returns accepted JSON.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
        r#"{"ok":true,"description":"write out.txt"}"#,
        r#"{"summary":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write out.txt")),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must recover from observation echo via protocol retry; got {:?}",
        output.result
    );
    let records = telemetry.records();
    // Only one ToolRequested event (for write_file) — the echo is NOT a tool call.
    let tool_requested_count = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ToolRequested { .. }))
        .count();
    assert_eq!(
        tool_requested_count, 1,
        "observation echo must not trigger ToolRequested; got {tool_requested_count}"
    );
    // The echo must trigger ParseFailed.
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, TelemetryEvent::ParseFailed { .. })),
        "observation echo must trigger ParseFailed"
    );
    // And ProtocolRetry.
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, TelemetryEvent::ProtocolRetry { .. })),
        "observation echo must trigger ProtocolRetry"
    );
}

// ── Completion pressure tests ────────────────────────────────────────────
