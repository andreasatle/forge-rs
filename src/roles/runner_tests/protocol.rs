use super::*;

#[test]
fn provider_error_maps_to_failed() {
    for (case, kind, message) in [
        ("Retryable", ProviderErrorKind::Retryable, "rate limited"),
        ("Terminal", ProviderErrorKind::Terminal, "auth error"),
    ] {
        let runner = ProviderRoleRunner::new(FailingProvider {
            kind,
            message: message.to_string(),
        });
        let result = runner
            .run_role(
                make_role_request(DeliberationRole::Producer, "write a poem"),
                &crate::telemetry::NoopTelemetry,
            )
            .result;
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "[{case}] provider error must map to Failed, got {result:?}"
        );
    }
}

#[test]
fn provider_role_runner_retries_malformed_json() {
    let provider = ScriptedProvider::from_strs(&[
        "invalid text",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let result = runner
        .run_role(
            producer_request("recover output"),
            &crate::telemetry::NoopTelemetry,
        )
        .result;

    assert!(matches!(result, RoleResult::Accepted { ref content } if content == "recovered"));
    assert_eq!(provider.requests.borrow().len(), 2);
}

#[test]
fn protocol_retry_prompt_preserves_context_without_leaking_raw_response() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let provider = ScriptedProvider::from_strs(&[
        "invalid text",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    runner.run_role(producer_request("recover output"), &telemetry);

    let records = telemetry.records();
    let parse_error = records
        .iter()
        .find_map(|record| match &record.event {
            TelemetryEvent::ParseFailed { parse_error, .. } => Some(parse_error.as_str()),
            _ => None,
        })
        .expect("protocol failure must emit ParseFailed telemetry");

    let requests = provider.requests.borrow();
    let retry_prompt = &requests[1].prompt;
    assert!(
        retry_prompt.contains(parse_error),
        "retry prompt must include the parser's actionable feedback; got:\n{retry_prompt}"
    );
    assert!(
        retry_prompt.contains("recover output"),
        "retry prompt must preserve the original objective; got:\n{retry_prompt}"
    );
    assert!(
        retry_prompt.contains("\"status\"") && retry_prompt.contains("$RESPONSE_SUMMARY"),
        "retry prompt must preserve the role response schema guidance; got:\n{retry_prompt}"
    );
    assert!(
        !retry_prompt.contains("$REASON_FOR_REJECTION"),
        "Work-node Producer retry prompt must never offer the rejected schema; got:\n{retry_prompt}"
    );
    assert!(
        !retry_prompt.contains("invalid text"),
        "retry prompt must not leak the raw invalid provider response; got:\n{retry_prompt}"
    );
    assert!(
        !retry_prompt.contains("\"...\""),
        "retry prompt must not reintroduce dot-placeholder JSON values; got:\n{retry_prompt}"
    );
}

#[test]
fn retry_limit_returns_failure() {
    let provider = ScriptedProvider::from_strs(&["invalid one", "invalid two", "invalid three"]);
    let runner = ProviderRoleRunner::new(&provider);

    let result = runner
        .run_role(
            producer_request("never valid"),
            &crate::telemetry::NoopTelemetry,
        )
        .result;

    assert!(matches!(result, RoleResult::Failed { .. }));
    assert_eq!(provider.requests.borrow().len(), 3);
}

#[test]
fn provider_role_runner_returns_semantic_rejection_without_retry() {
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs revision"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let result = runner
        .run_role(
            referee_request("review output", "draft", "review"),
            &crate::telemetry::NoopTelemetry,
        )
        .result;

    assert!(
        matches!(result, RoleResult::Rejected { ref reason } if reason == "needs revision"),
        "semantic rejection must not retry, got {result:?}"
    );
    assert_eq!(provider.requests.borrow().len(), 1);
}

#[test]
fn role_runner_uses_provider_response_content() {
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"the result"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        producer_request("produce something"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "the result"),
        "role runner must use response.content; got {:?}",
        output.result
    );
}

// ── policy: critic write request produces error observation ──────────────

#[test]
fn role_runner_uses_configured_max_tokens() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_max_tokens(&provider, 256);

    runner.run_role(producer_request("test"), &crate::telemetry::NoopTelemetry);

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].max_tokens, 256,
        "configured max_tokens must be forwarded to the provider"
    );
}

#[test]
fn scripted_provider_supports_request_response_objects() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("anything"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0].prompt.is_empty(),
        "request must carry a prompt"
    );
    assert_eq!(
        requests[0].max_tokens, MAX_RESPONSE_TOKENS,
        "request must carry the runner's max_tokens constant"
    );
}

#[test]
fn role_runner_requests_json_output() {
    use crate::providers::StructuredOutput;

    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("write something"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Json),
        "RoleRunner must request Json structured output"
    );
}

// ── prompt/policy consistency ────────────────────────────────────────────

#[test]
fn tool_request_detection_still_works_with_no_preamble() {
    // Tool requests starting with { are still detected and produce an error observation
    // (since tool_context is None), then the model returns a clean result.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"listed files"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(producer_request("test"), &crate::telemetry::NoopTelemetry);

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "tool request without context must continue to final result; got {:?}",
        output.result
    );
    assert_eq!(provider.requests.borrow().len(), 2);
    assert!(
        provider.requests.borrow()[1]
            .prompt
            .contains("no file tools available"),
        "second prompt must include error observation from tool attempt"
    );
}

#[test]
fn preamble_triggers_retry_in_runner_loop() {
    // Preamble causes parse failure; on retry the model returns clean JSON.
    let provider = ScriptedProvider::from_strs(&[
        "Here is the result:\n{\"status\":\"accepted\",\"content\":\"draft\"}",
        r#"{"status":"accepted","content":"recovered"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        producer_request("produce output"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "recovered"),
        "clean JSON on retry must succeed; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must retry once after preamble failure"
    );
}

#[test]
fn planner_accepts_valid_planner_output() {
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "valid PlannerOutput must be accepted without retry; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        1,
        "no retry needed for valid PlannerOutput"
    );
}

#[test]
fn planner_retries_invalid_planner_output() {
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Here is my plan: do things step by step."}"#,
        tasks_json,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "valid PlannerOutput on retry must succeed; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must retry once for invalid planner content"
    );
}

#[test]
fn planner_rejects_prose_content_in_coding_mode() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Plan: first do this, then that."}"#,
        r#"{"status":"accepted","content":"Revised plan: still prose."}"#,
        r#"{"status":"accepted","content":"Final prose attempt."}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "prose planner content must fail after retries exhausted; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "must attempt initial + MAX_PROTOCOL_RETRIES = 3 total calls"
    );
}

// ── Step 3: preamble detection ────────────────────────────────────────────

#[test]
fn planner_output_fallback_no_longer_hides_invalid_plan() {
    // Prose content that used to silently fall back to a single work node
    // now triggers retry and eventual failure.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Do the task however you like."}"#,
        r#"{"status":"accepted","content":"Still prose."}"#,
        r#"{"status":"accepted","content":"More prose."}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "invalid planner content must no longer fall back silently; got {:?}",
        output.result
    );
}

// ── New direct-planner-output tests ──────────────────────────────────────

#[test]
fn invalid_direct_planner_output_retries() {
    // Parses as PlannerOutput but has a self-dependency — validation must retry.
    let invalid_json = r#"{"tasks":[{"id":"loop","objective":"do loop","operation":"modify","targets":["loop.txt"],"depends_on":["loop"]}]}"#;
    let valid_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[invalid_json, valid_json]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "must accept valid plan after retrying invalid one; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must retry once for validation failure"
    );
}

#[test]
fn planner_does_not_require_content_string_starting_with_brace() {
    // Regression: live failure produced {"status":"accepted","content":"{"}
    // which must fail cleanly, not produce PlanAccepted.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"{"}"#,
        r#"{"status":"accepted","content":"{"}"#,
        r#"{"status":"accepted","content":"{"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "status/content wrapper with truncated inner JSON must fail; got {:?}",
        output.result
    );
}
