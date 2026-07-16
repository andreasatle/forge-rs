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
    let provider = ScriptedProvider::from_strs(&["invalid text", r#"{"summary":"recovered"}"#]);
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
fn protocol_retry_prompt_preserves_context_and_shows_raw_response() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let provider = ScriptedProvider::from_strs(&["invalid text", r#"{"summary":"recovered"}"#]);
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
        retry_prompt.contains("`summary` must be a non-empty task-specific string"),
        "retry prompt must preserve the role response schema guidance; got:\n{retry_prompt}"
    );
    assert!(
        !retry_prompt.contains("`status`"),
        "Work-node Producer retry prompt must never offer the status/content schema; got:\n{retry_prompt}"
    );
    assert!(
        retry_prompt.contains("invalid text"),
        "retry prompt must show the model its own raw invalid response; got:\n{retry_prompt}"
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
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"the result"}"#]);
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
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
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
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
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
fn role_runner_requests_schema_specific_grammar_output() {
    use crate::providers::StructuredOutput;
    use crate::roles::policy::PRODUCER_GBNF;

    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("write something"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Grammar(PRODUCER_GBNF.to_string())),
        "RoleRunner must request the Work-node Producer's GBNF grammar, not generic JSON"
    );
}

#[test]
fn tool_loop_uses_union_grammar_then_narrows_to_final_response_grammar() {
    // Invariant: while a Work-node Producer still has tool budget, every
    // provider call must be constrained by the tool-call-or-final-response
    // union grammar, not the final-response-only grammar — otherwise a real
    // GBNF-constrained provider could never emit a valid tool call. Once a
    // write triggers completion pressure, the grammar must narrow to
    // PRODUCER_GBNF so the model is forced to return its summary.
    use crate::providers::StructuredOutput;
    use crate::roles::policy::PRODUCER_TOOL_GBNF;

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"out.txt","content":"hello"}"#,
        r#"{"summary":"wrote out.txt"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Grammar(PRODUCER_TOOL_GBNF.to_string())),
        "first call must use the union tool-or-summary grammar while tools remain available"
    );
    assert_eq!(
        requests[1].output_schema,
        Some(StructuredOutput::Grammar(PRODUCER_GBNF.to_string())),
        "call after a recorded write (completion pressure) must narrow to the summary-only grammar"
    );
}

#[test]
fn reviewer_tool_loop_uses_union_grammar_then_narrows_to_final_response_grammar() {
    // Same invariant as the Producer tool loop, but for Critic/Referee: the
    // union grammar must allow read_file/list_files plus accept-or-reject
    // while tools remain available, then narrow to ROLE_GBNF once decision
    // pressure (the read-only tool budget, MAX_READ_ONLY_TOOL_STEPS = 2) ends
    // the tool loop.
    use crate::providers::StructuredOutput;
    use crate::roles::policy::REVIEWER_TOOL_GBNF;

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"status":"accepted","content":"looks correct"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(critic_request("review the work", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "provider must be called three times");
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Grammar(REVIEWER_TOOL_GBNF.to_string())),
        "first call must use the union tool-or-decision grammar while tools remain available"
    );
    assert_eq!(
        requests[1].output_schema,
        Some(StructuredOutput::Grammar(REVIEWER_TOOL_GBNF.to_string())),
        "second call must still use the union grammar; read-only budget not yet exhausted"
    );
    assert_eq!(
        requests[2].output_schema,
        Some(StructuredOutput::Grammar(ROLE_GBNF.to_string())),
        "call after decision pressure must narrow to the accept-or-reject-only grammar"
    );
}

// ── prompt/policy consistency ────────────────────────────────────────────

#[test]
fn preamble_triggers_retry_in_runner_loop() {
    // Preamble causes parse failure; on retry the model returns clean JSON.
    let provider = ScriptedProvider::from_strs(&[
        "Here is the result:\n{\"summary\":\"draft\"}",
        r#"{"summary":"recovered"}"#,
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
fn planner_producer_grammar_and_footer_forbid_work_without_worker_roles() {
    // Invariant: a Plan node under an adapter with no worker roles (e.g. a
    // pure decomposition adapter with no `workers:` configured) must never
    // be offered `kind: "work"` — there is no role to assign a work task to,
    // so only "plan"/"task" are grammar-legal, and `kind` must be stated
    // explicitly rather than defaulting to "work".
    use crate::roles::policy::PLANNER_GBNF_NO_WORK;

    let response = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"do the thing","name":"thing","function_name":"thing","role_targets":[{"role":"implementer","file_path":"thing.txt"}],"operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[response]);
    let runner = ProviderRoleRunner::new(&provider);
    let request = RoleRequest {
        node_kind: NodeKind::Plan,
        ..plan_request("plan the work")
    };

    let output = runner.run_role(request, &crate::telemetry::NoopTelemetry);
    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "valid PlannerOutput must be accepted; got {:?}",
        output.result
    );

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Grammar(PLANNER_GBNF_NO_WORK.to_string())),
        "a workerless adapter must request the no-work grammar"
    );
    assert!(
        requests[0]
            .prompt
            .contains("`kind: \"work\"` is not available"),
        "prompt must tell the model kind: work is unavailable; got:\n{}",
        requests[0].prompt
    );
    assert!(
        !requests[0].prompt.contains("Required `role` field"),
        "a workerless adapter's prompt must never mention assigning a role; got:\n{}",
        requests[0].prompt
    );
}

#[test]
fn planner_producer_grammar_and_footer_offer_work_with_worker_roles() {
    // Invariant: a Plan node under an adapter that defines worker roles
    // keeps the with-operation, with-roles grammar and footer — `kind:
    // "work"` remains available and `role` is required.
    use crate::roles::policy::PLANNER_GBNF_WITH_ROLES;

    let response = r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","role":"implementer","targets":["thing.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[response]);
    let policy = RolePolicy {
        worker_role_descriptions: vec![(
            "implementer".to_string(),
            "Implements code changes.".to_string(),
        )],
        ..RolePolicy::default()
    };
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);
    let request = RoleRequest {
        node_kind: NodeKind::Plan,
        ..plan_request("plan the work")
    };

    let output = runner.run_role(request, &crate::telemetry::NoopTelemetry);
    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "valid PlannerOutput must be accepted; got {:?}",
        output.result
    );

    let requests = provider.requests.borrow();
    assert_eq!(
        requests[0].output_schema,
        Some(StructuredOutput::Grammar(
            PLANNER_GBNF_WITH_ROLES.to_string()
        )),
        "an adapter with worker roles must request the with-roles grammar"
    );
    assert!(
        requests[0].prompt.contains("Required `role` field"),
        "prompt must contain the node-kind-specific protocol footer; got:\n{}",
        requests[0].prompt
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
    // Regression: prose planner content used to silently fall back to a
    // single work node; it must now retry and fail instead.
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
