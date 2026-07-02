use super::*;

#[test]
fn work_reviewer_prompt_guides_read_file_to_declared_target() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(with_target_files(
            critic_request("Review the update.", "updated main.py"),
            &["main.py"],
        )),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains(r#"{"tool":"read_file","path":"main.py"}"#),
        "Work reviewer prompt must guide read_file to declared target; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("write_file"),
        "Work reviewer prompt must remain read-only; got:\n{prompt}"
    );
}

// ── RolePolicy tests ─────────────────────────────────────────────────────

#[test]
fn work_reviewer_must_read_file_before_accepting() {
    // Reviewer (Critic) first accepts without reading; enforcement fires and
    // a retry prompt is issued.  On the retry the reviewer calls read_file,
    // then accepts.  The final result must be Accepted.
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let (_temp, view) = make_view("reviewer-read-enforce");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"confirmed after reading"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "reviewer must eventually accept after reading; got {:?}",
        output.result
    );
    let records = telemetry.into_records();
    let retries: Vec<_> = records
        .iter()
        .filter(|r| matches!(r.event, TelemetryEvent::ProtocolRetry { .. }))
        .collect();
    assert_eq!(
        retries.len(),
        1,
        "exactly one ProtocolRetry must be emitted for the enforcement violation"
    );
}

#[test]
fn work_reviewer_exhausts_retries_without_reading_fails() {
    // Reviewer accepts without reading on every attempt; after
    // MAX_PROTOCOL_RETRIES+1 tries the role must fail.
    let (_temp, view) = make_view("reviewer-exhaust-retries");
    let responses =
        vec![r#"{"status":"accepted","content":"looks good"}"#; MAX_PROTOCOL_RETRIES + 2];
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { .. }),
        "reviewer that never reads must fail after exhausting retries; got {:?}",
        output.result
    );
    if let RoleResult::Failed { reason, .. } = &output.result {
        assert!(
            reason.contains("reading"),
            "failure reason must mention reading; got: {reason}"
        );
    }
}

#[test]
fn plan_reviewer_can_accept_without_reading_files() {
    // Plan-node reviewers judge structure, not file contents.
    // The read-file enforcement must NOT apply to them.
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"plan is sound"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        RoleRequest {
            node_kind: NodeKind::Plan,
            ..critic_request("review the plan", "plan output")
        },
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "plan reviewer must accept without needing to read a file; got {:?}",
        output.result
    );
}

#[test]
fn work_reviewer_without_tool_context_can_accept() {
    // When tool_context is None the reviewer has no file tools; the
    // read-file enforcement must not apply in that case.
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        referee_request("approve the result", "content", "review"),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "reviewer without tool context must accept without enforcement; got {:?}",
        output.result
    );
}

// ── read-file enforcement regression tests ────────────────────────────────

#[test]
fn failed_read_file_does_not_satisfy_enforcement() {
    // A read_file that returns a failure (absolute path escapes workspace root)
    // must NOT set read_file_executed. The enforcement must fire even though
    // read_file was attempted, and the error message must include the role name
    // and the count of failed attempts.
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let (_temp, view) = make_view("failed-read-enforcement");
    let provider = ScriptedProvider::from_strs(&[
        // Critic attempts read_file with an absolute path → fails (escapes workspace)
        r#"{"tool":"read_file","path":"/absolute/path/that/escapes"}"#,
        // Critic accepts without having successfully read → enforcement fires
        r#"{"status":"accepted","content":"looks good to me here"}"#,
        // After enforcement retry the Critic reads the valid file
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        // Critic accepts after successful read
        r#"{"status":"accepted","content":"confirmed after reading the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &telemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "Critic must accept after retry with successful read; got {:?}",
        output.result
    );
    let records = telemetry.into_records();
    let parse_failed = records
        .iter()
        .find(|r| matches!(r.event, TelemetryEvent::ParseFailed { .. }));
    assert!(
        parse_failed.is_some(),
        "ParseFailed must be emitted when read_file was attempted but failed"
    );
    if let Some(r) = parse_failed
        && let TelemetryEvent::ParseFailed { parse_error, .. } = &r.event
    {
        assert!(
            parse_error.contains("Critic"),
            "error must name the role; got: {parse_error}"
        );
        assert!(
            parse_error.contains("1 read_file attempt(s) were made but all failed"),
            "error must report failed attempt count; got: {parse_error}"
        );
    }
}

#[test]
fn failed_reads_exhaust_budget_enforcement_fails_directly() {
    // Two read_file calls fail with different errors (no repeated-obs coercion),
    // exhausting the reviewer tool budget (decision pressure fires).
    // The Critic then accepts → enforcement fires with final_response_only=true.
    // The fix: enforcement must fail DIRECTLY rather than issuing a must-read
    // retry that would contradict the blocked-tool state.
    // Outcome: exactly 3 provider calls (not 5+), clear failure message.
    let (_temp, view) = make_view("failed-reads-budget");
    let provider = ScriptedProvider::from_strs(&[
        // Read 1: absolute path → "path escapes the workspace root"
        r#"{"tool":"read_file","path":"/absolute/path"}"#,
        // Read 2: relative non-existent path → "file not found"
        // Different observation from Read 1 → no repeated-obs coercion.
        // read_only_tool_steps reaches MAX_READ_ONLY_TOOL_STEPS → decision pressure.
        r#"{"tool":"read_file","path":"nonexistent.txt"}"#,
        // Critic accepts under decision pressure → enforcement fires,
        // final_response_only=true → fail directly (no must-read retry issued).
        r#"{"status":"accepted","content":"looks good to me here"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &crate::telemetry::NoopTelemetry,
    );

    let RoleResult::Failed { reason, .. } = &output.result else {
        panic!(
            "Critic must fail when all reads failed and tool budget exhausted; got {:?}",
            output.result
        );
    };
    assert!(
        reason.contains("Critic"),
        "failure reason must name the role; got: {reason}"
    );
    assert!(
        reason.contains("2 read_file attempt(s) were made but all failed"),
        "failure reason must report failed attempt count; got: {reason}"
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "must be exactly 3 provider calls (no extra must-read retry after decision pressure)"
    );
}

#[test]
fn read_file_flag_survives_protocol_retry() {
    // Critic reads successfully (read_file_executed set), then returns bad JSON
    // triggering a protocol retry, then accepts. The read flag must survive the
    // protocol retry so that enforcement does not fire on the final accept.
    let (_temp, view) = make_view("read-flag-survives-retry");
    let provider = ScriptedProvider::from_strs(&[
        // Critic reads hello.txt → FileContents → read_file_executed = true
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        // Protocol failure (response does not start with '{')
        "not valid json at all here",
        // Critic accepts after protocol retry; flag was set → no enforcement
        r#"{"status":"accepted","content":"confirmed after reading the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(critic_request("review the work", "some content"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "Critic must accept when read flag survived protocol retry; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "provider must be called exactly 3 times (read + bad-json retry + final)"
    );
}

#[test]
fn referee_read_file_satisfies_enforcement() {
    // Referee must also read at least one file before accepting on Work nodes.
    // A single successful read must satisfy the enforcement for the Referee role.
    let (_temp, view) = make_view("referee-read-satisfies");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"referee confirmed the file contents"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            referee_request("approve the work", "content", "review"),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "Referee must accept after a successful read_file; got {:?}",
        output.result
    );
}
