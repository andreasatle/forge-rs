use super::*;

#[test]
fn producer_completion_pressure_fires_after_write_file() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#,
        r#"{"status":"accepted","content":"wrote the file successfully"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "producer must finalize after write_file; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("The requested change has already been recorded"),
        "second prompt must contain completion-pressure hint; got:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("Available file tools:"),
        "second prompt must not advertise tools after completion pressure; got:\n{second_prompt}"
    );
}

#[test]
fn successful_replace_text_observation_instructs_final_response() {
    // hello.txt from make_view contains "hello world\n"
    let (_temp, view) = make_view("replace-text-final");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"replace_text","path":"hello.txt","old":"hello world","new":"goodbye"}"#,
        r#"{"status":"accepted","content":"replaced hello with goodbye"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_tool_context(
            producer_request("replace hello with goodbye in hello.txt"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "must call provider twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("The requested change has already been recorded."),
        "successful replace_text must include completion-pressure text; got:\n{second_prompt}"
    );
    assert!(
        second_prompt.contains("Do not call any more tools."),
        "successful replace_text must prohibit further tool calls; got:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("Available file tools:"),
        "completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
    );
}

#[test]
fn successful_mutation_tool_observation_instructs_final_response() {
    for (case, tool_response, final_response, objective) in [
        (
            "write_file",
            r#"{"tool":"write_file","path":"result.txt","content":"some output"}"#,
            r#"{"status":"accepted","content":"wrote result.txt"}"#,
            "write result.txt",
        ),
        (
            "delete_file",
            r#"{"tool":"delete_file","path":"old.txt"}"#,
            r#"{"status":"accepted","content":"deleted old.txt"}"#,
            "delete old.txt",
        ),
    ] {
        let provider = ScriptedProvider::from_strs(&[tool_response, final_response]);
        let runner = ProviderRoleRunner::new(&provider);
        let (_temp, view) = make_view_with_entries(
            &format!("completion-pressure-{case}"),
            &[
                ("hello.txt", b"hello world\n".as_slice()),
                ("old.txt", b"delete me\n".as_slice()),
            ],
        );

        runner.run_role(
            with_tool_context(producer_request(objective), view),
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2, "[{case}] must call provider twice");
        let second_prompt = &requests[1].prompt;
        assert!(
            second_prompt.contains("The requested change has already been recorded."),
            "[{case}] successful {case} must include completion-pressure text; got:\n{second_prompt}"
        );
        assert!(
            second_prompt.contains("Do not call any more tools."),
            "[{case}] successful {case} must prohibit further tool calls; got:\n{second_prompt}"
        );
        assert!(
            !second_prompt.contains("Available file tools:"),
            "[{case}] completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
        );
    }
}

#[test]
fn read_file_after_mutation_is_completion_pressure_violation() {
    // Sequence: write_file (mutation → CP), read_file (CP violation → retry),
    // accepted. After completion pressure is active, any tool request —
    // including read_file — is treated as a protocol violation.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"data.txt","content":"hello"}"#,
        r#"{"tool":"read_file","path":"data.txt"}"#,
        r#"{"status":"accepted","content":"wrote data.txt"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("write data.txt")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "must call provider three times");
    // The third prompt must include the violation note ("tools are no longer available")
    // and must NOT include the tool section (CP rebuilt prompt from core).
    let third_prompt = &requests[2].prompt;
    assert!(
        third_prompt.contains("Tools are no longer available."),
        "read_file during CP must produce violation note; got:\n{third_prompt}"
    );
    assert!(
        !third_prompt.contains("Available file tools:"),
        "CP violation prompt must not contain the tool section; got:\n{third_prompt}"
    );
}

#[test]
fn completion_pressure_hides_tool_section() {
    // After a successful mutation the prompt must not contain the tool section.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("write out.txt")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let second_prompt = &requests[1].prompt;
    assert!(
        !second_prompt.contains("Available file tools:"),
        "completion-pressure prompt must not include the tool section; got:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("write_file"),
        "completion-pressure prompt must not list write_file; got:\n{second_prompt}"
    );
}

#[test]
fn tool_request_after_completion_pressure_is_rejected() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // Sequence: write_file (mutation → CP), list_files (CP violation → retry),
    // accepted (final response).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"data"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write out.txt")),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must accept after CP violation is retried; got {:?}",
        output.result
    );
    assert_eq!(provider.requests.borrow().len(), 3);

    let records = telemetry.records();
    // list_files during CP must NOT emit ToolRequested.
    let tool_requested_count = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ToolRequested { .. }))
        .count();
    assert_eq!(
        tool_requested_count, 1,
        "only write_file must fire ToolRequested; CP violation must not; got {tool_requested_count}"
    );
    // CP violation must emit ParseFailed and ProtocolRetry.
    assert!(
        records.iter().any(
            |r| matches!(&r.event, TelemetryEvent::ParseFailed { parse_error, .. }
                    if parse_error.contains("no tools are available"))
        ),
        "CP violation must emit ParseFailed with 'no tools are available'"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, TelemetryEvent::ProtocolRetry { .. })),
        "CP violation must emit ProtocolRetry"
    );
}

#[test]
fn worker_can_return_accepted_after_completion_pressure() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"output data"}"#,
        r#"{"status":"accepted","content":"wrote result.txt with output data"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write result.txt")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content }
                if content == "wrote result.txt with output data"),
        "worker must be able to return Accepted after CP; got {:?}",
        output.result
    );
    assert!(
        output.artifact_changed,
        "write_file must mark the WorkAttempt workspace as changed"
    );
}

#[test]
fn planner_not_affected_by_completion_pressure() {
    // Plan+Producer: even if the planner returns a mutation-like tool request
    // (which it shouldn't, since tool_context is None), completion pressure
    // must never activate. Here we verify that the Planner takes the direct
    // PlannerOutput path without any CP interference.
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "planner must succeed without CP interference; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        1,
        "planner must complete in one call"
    );
    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        !prompt.contains("Do not call any more tools."),
        "planner prompt must not contain CP instruction; got:\n{prompt}"
    );
}

#[test]
fn critic_not_affected_by_completion_pressure() {
    // Critic role: even with tool context, CP must never activate (Critic is not Producer).
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Rejected { .. }),
        "critic must succeed without CP interference; got {:?}",
        output.result
    );
    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        !prompt.contains("Do not call any more tools."),
        "critic prompt must not contain CP instruction; got:\n{prompt}"
    );
}

#[test]
fn referee_not_affected_by_completion_pressure() {
    // Referee role: CP must never activate (Referee is not Producer).
    // Referee must read a file before accepting (enforcement); use a real
    // view so read_file("hello.txt") returns FileContents.
    let (_temp, view) = make_view("referee-no-cp");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            referee_request("approve the result", "content", "review"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "referee must succeed without CP interference; got {:?}",
        output.result
    );
    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        !prompt.contains("Do not call any more tools."),
        "referee prompt must not contain CP instruction; got:\n{prompt}"
    );
}

// ── write_file example hardening ─────────────────────────────────────────
