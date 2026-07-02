use super::*;

#[test]
fn critic_decision_pressure_fires_after_max_read_steps() {
    let (_temp, view) = make_view("critic-decision-pressure");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"the file looks good"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review hello.txt", "draft"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "critic must finalize after read_file steps; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "provider must be called three times");
    let third_prompt = &requests[2].prompt;
    assert!(
        third_prompt.contains("sufficient evidence"),
        "third prompt must contain decision-pressure text; got:\n{third_prompt}"
    );
}

#[test]
fn critic_enters_decision_pressure_after_max_read_only_steps() {
    // After exactly MAX_READ_ONLY_TOOL_STEPS tool observations Critic must
    // receive a decision-pressure observation and then return a final result.
    let mut responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; MAX_READ_ONLY_TOOL_STEPS];
    responses.push(r#"{"status":"rejected","reason":"files look insufficient for the task"}"#);
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft content")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Rejected { .. }),
        "critic must return final result after decision pressure; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(
        requests.len(),
        MAX_READ_ONLY_TOOL_STEPS + 1,
        "provider must be called MAX_READ_ONLY_TOOL_STEPS + 1 times"
    );
    let last_prompt = &requests[MAX_READ_ONLY_TOOL_STEPS].prompt;
    assert!(
        last_prompt.contains("sufficient evidence"),
        "decision-pressure prompt must mention 'sufficient evidence'; got:\n{last_prompt}"
    );
    assert!(
        last_prompt.contains("Do not call any more tools."),
        "decision-pressure prompt must prohibit further tools; got:\n{last_prompt}"
    );
}

#[test]
fn referee_enters_decision_pressure_after_max_read_only_steps() {
    // Referee reads a file (step 1) then lists files (step 2 → DP fires),
    // then accepts.  The read_file call satisfies the file-read enforcement
    // and the tool-step count still hits MAX_READ_ONLY_TOOL_STEPS.
    let (_temp, view) = make_view("referee-dp-steps");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"reviewed all evidence and approved"}"#,
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
        "referee must return final result after decision pressure; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(
        requests.len(),
        MAX_READ_ONLY_TOOL_STEPS + 1,
        "provider must be called MAX_READ_ONLY_TOOL_STEPS + 1 times"
    );
    let last_prompt = &requests[MAX_READ_ONLY_TOOL_STEPS].prompt;
    assert!(
        last_prompt.contains("sufficient evidence"),
        "decision-pressure prompt must mention 'sufficient evidence'; got:\n{last_prompt}"
    );
}

#[test]
fn critic_decision_pressure_hides_tool_section() {
    let mut responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; MAX_READ_ONLY_TOOL_STEPS];
    responses
        .push(r#"{"status":"rejected","reason":"cannot determine quality without more context"}"#);
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let pressure_prompt = &requests[MAX_READ_ONLY_TOOL_STEPS].prompt;
    assert!(
        !pressure_prompt.contains("Available file tools:"),
        "decision-pressure prompt must not include the tool section; got:\n{pressure_prompt}"
    );
    assert!(
        !pressure_prompt.contains("list_files"),
        "decision-pressure prompt must not list file tools; got:\n{pressure_prompt}"
    );
}

#[test]
fn critic_decision_pressure_rejects_further_tool_calls() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    // After MAX_READ_ONLY_TOOL_STEPS observations the (MAX+1)-th tool call
    // must be a protocol violation, then the model returns a final result.
    let mut responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; MAX_READ_ONLY_TOOL_STEPS];
    responses.push(r#"{"tool":"list_files"}"#); // violation
    responses.push(r#"{"status":"rejected","reason":"output does not meet requirements"}"#);
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Rejected { .. }),
        "critic must reject after CP violation is retried; got {:?}",
        output.result
    );
    let records = telemetry.records();
    // The tool call after pressure must NOT emit ToolRequested.
    let tool_requested_count = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ToolRequested { .. }))
        .count();
    assert_eq!(
        tool_requested_count, MAX_READ_ONLY_TOOL_STEPS,
        "only the first {MAX_READ_ONLY_TOOL_STEPS} tool calls must emit ToolRequested; got {tool_requested_count}"
    );
    // Violation must emit ParseFailed with 'no tools are available'.
    assert!(
        records.iter().any(
            |r| matches!(&r.event, TelemetryEvent::ParseFailed { parse_error, .. }
                    if parse_error.contains("no tools are available"))
        ),
        "decision-pressure violation must emit ParseFailed with 'no tools are available'"
    );
}

#[test]
fn producer_not_affected_by_decision_pressure() {
    // Producer may use more than MAX_READ_ONLY_TOOL_STEPS distinct read-only
    // tool calls without entering decision pressure (which only applies to
    // Critic and Referee). Each read targets a different file so no repeated-
    // observation coercion fires either.
    let read_count = MAX_READ_ONLY_TOOL_STEPS + 1;
    let (_temp, view) = make_view_with_n_files("producer-no-dp", read_count);
    let mut responses: Vec<String> = (0..read_count)
        .map(|i| format!(r#"{{"tool":"read_file","path":"file{i}.txt"}}"#))
        .collect();
    responses.push(r#"{"summary":"produced the required output"}"#.to_string());
    let response_strs: Vec<&str> = responses.iter().map(|s| s.as_str()).collect();
    let provider = ScriptedProvider::from_strs(&response_strs);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("read files and produce output"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "producer must succeed with more than MAX_READ_ONLY_TOOL_STEPS distinct reads; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        read_count + 1,
        "producer must be allowed {read_count} distinct tool calls"
    );
    // None of the prompts must contain decision-pressure text.
    for (i, req) in provider.requests.borrow().iter().enumerate() {
        assert!(
            !req.prompt.contains("sufficient evidence"),
            "producer prompt[{i}] must not contain decision-pressure text; got:\n{}",
            req.prompt
        );
    }
}

#[test]
fn read_only_tool_steps_counter_is_per_invocation() {
    // Each invocation starts with a fresh counter. Two separate invocations
    // each with MAX_READ_ONLY_TOOL_STEPS - 1 tool steps must not trigger pressure.
    for _ in 0..2 {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"list_files"}"#,
            r#"{"status":"rejected","reason":"draft does not satisfy the requirements"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            with_dummy_tool_context(critic_request("review the draft", "draft")),
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Rejected { .. }),
            "critic with 1 tool step must succeed without decision pressure; got {:?}",
            output.result
        );
        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 2, "provider must be called twice");
        let second_prompt = &requests[1].prompt;
        assert!(
            !second_prompt.contains("sufficient evidence"),
            "second prompt must not contain decision-pressure text; got:\n{second_prompt}"
        );
    }
}

// ── read-file enforcement tests ───────────────────────────────────────────
