use super::*;

#[test]
fn role_runner_executes_read_file_tool_then_accepts() {
    let (_temp, view) = make_view("read-file-tool");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"read the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("read hello.txt"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "read the file"),
        "expected Accepted after read_file tool loop, got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        2,
        "must call provider twice"
    );
    let second_prompt = &provider.requests.borrow()[1].prompt;
    assert!(
        second_prompt.contains("Framework tool observation:"),
        "second prompt must include tool observation"
    );
    assert!(
        second_prompt.contains("hello world"),
        "observation must include file content"
    );
}

#[test]
fn role_runner_records_workspace_write() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#,
        r#"{"summary":"wrote the file"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("write a file")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "expected Accepted, got {:?}",
        output.result
    );
    assert!(
        output.artifact_changed,
        "write_file must mark the WorkAttempt workspace as changed"
    );
}

#[test]
fn role_runner_rejects_tool_when_no_artifact_view() {
    // Any role with no tool_context (Producer with no view, or a Plan-node
    // request where tool_context is always None) must turn a tool request
    // into a "no file tools available" error observation and still reach a
    // final result.
    for (case, request, tool_call, final_response) in [
        (
            "producer",
            producer_request("do the thing"),
            r#"{"tool":"list_files"}"#,
            r#"{"summary":"used no tools"}"#,
        ),
        (
            "planner",
            plan_request("plan the work"),
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"tasks":[{"id":"t1","objective":"do the thing","operation":"modify","targets":["thing.txt"],"depends_on":[]}]}"#,
        ),
    ] {
        let provider = ScriptedProvider::from_strs(&[tool_call, final_response]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(request, &crate::telemetry::NoopTelemetry);

        assert!(
            matches!(output.result, RoleResult::Accepted { .. }),
            "[{case}] tool request without view must produce error observation and allow final result; got {:?}",
            output.result
        );
        assert_eq!(provider.requests.borrow().len(), 2, "[{case}]");
        let second_prompt = &provider.requests.borrow()[1].prompt;
        assert!(
            second_prompt.contains("no file tools available"),
            "[{case}] second prompt must include error observation"
        );
    }
}

#[test]
fn role_runner_stops_at_tool_loop_limit() {
    // Repeated identical list_files calls produce repeated identical observations,
    // so repeated-observation coercion fires after 2 calls and the 3rd call
    // (while coercion is active) immediately fails with a specific protocol error.
    let responses: Vec<&str> = vec![r#"{"tool":"list_files"}"#; 3];
    let provider = ScriptedProvider::from_strs(&responses);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("loop forever")),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { ref reason, .. } if reason.contains("repeated")),
        "must fail with repeated-observation error before the generic limit; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "provider must be called exactly 3 times (2 duplicate observations + 1 post-coercion tool call)"
    );
}

// Helper: build a bare repo containing `n` distinct files named file0.txt .. file{n-1}.txt.

#[test]
fn role_runner_generic_tool_loop_limit_applies_without_repetition() {
    // MAX_TOOL_STEPS distinct read_file requests each produce unique content;
    // no repeated observation fires. The (MAX_TOOL_STEPS+1)-th call hits the
    // generic loop limit.
    let (_temp, view) = make_view_with_n_files("generic-limit", MAX_TOOL_STEPS);
    let responses: Vec<String> = (0..=MAX_TOOL_STEPS)
        .map(|i| format!(r#"{{"tool":"read_file","path":"file{i}.txt"}}"#))
        .collect();
    let response_strs: Vec<&str> = responses.iter().map(|s| s.as_str()).collect();
    let provider = ScriptedProvider::from_strs(&response_strs);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("loop with distinct files"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Failed { ref reason, .. } if reason.contains("tool loop limit")),
        "must fail with generic tool loop limit when observations are distinct; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        MAX_TOOL_STEPS + 1,
        "provider must be called exactly MAX_TOOL_STEPS + 1 times"
    );
}

// ── repeated-observation coercion tests ──────────────────────────────────

#[test]
fn producer_repeated_identical_read_file_triggers_coercion() {
    let (_temp, view) = make_view("repeated-read-coercion");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"summary":"read the same file twice"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("inspect hello.txt"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "producer must accept after coercion forces final response; got {:?}",
        output.result
    );
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 3, "provider must be called three times");
    let third_prompt = &requests[2].prompt;
    assert!(
        third_prompt.contains("You have already inspected this information"),
        "third prompt must contain repeated-observation coercion text; got:\n{third_prompt}"
    );
    assert!(
        !third_prompt.contains("Available file tools:"),
        "third prompt must not advertise tools after coercion; got:\n{third_prompt}"
    );
}

#[test]
fn repeated_identical_tool_calls_fail_before_generic_limit() {
    // The producer keeps calling list_files with identical results. The second
    // identical observation triggers coercion. A third tool call (after coercion)
    // fails immediately with a specific protocol error — not the generic limit.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"tool":"list_files"}"#,
        r#"{"tool":"list_files"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(producer_request("loop on list_files")),
        &crate::telemetry::NoopTelemetry,
    );

    let reason = match &output.result {
        RoleResult::Failed { reason, .. } => reason.clone(),
        other => panic!("expected Failed, got {other:?}"),
    };
    assert!(
        reason.contains("repeated"),
        "failure reason must mention 'repeated'; got: {reason}"
    );
    assert!(
        !reason.contains("tool loop limit"),
        "failure must not use generic tool loop limit message; got: {reason}"
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "only 3 provider calls: duplicate observation fires at call 2, coercion violation at call 3"
    );
}

#[test]
fn existing_valid_tool_use_still_works() {
    // list_files then write_file then accepted — no repeated observations,
    // no coercion, normal completion pressure after the write.
    let (_temp, view) = make_view("valid-tool-use");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"list_files"}"#,
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        r#"{"summary":"listed files and wrote result"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(producer_request("list then write"), view),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { ref content } if content == "listed files and wrote result"),
        "valid tool sequence must succeed; got {:?}",
        output.result
    );
    assert_eq!(
        provider.requests.borrow().len(),
        3,
        "all 3 provider calls must be made"
    );
}

// ── placeholder tool echo tests ─────────────────────────────────────────

#[test]
fn critic_write_request_produces_error_observation() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"critic draft"}"#,
        r#"{"status":"rejected","reason":"cannot write"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review the work", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    // The role must continue (not crash) and the second prompt must include
    // the permission error observation.
    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("not permitted"),
        "second prompt must include write-permission error; got:\n{second_prompt}"
    );
    assert!(
        !output.artifact_changed,
        "critic write must not mark the workspace changed"
    );
}

// ── observation bounding ─────────────────────────────────────────────────

#[test]
fn format_tool_observation_is_bounded() {
    let large_content = "x".repeat(500);
    let response = FileToolResponse::FileContents {
        path: "big.txt".to_owned(),
        content: large_content,
    };
    let max_obs = 100;
    let observation = format_tool_observation(&response, max_obs);
    assert!(
        observation.len() <= max_obs + "\n[observation truncated]".len(),
        "observation must be bounded; len={}, max={}",
        observation.len(),
        max_obs
    );
    assert!(
        observation.contains("[observation truncated]"),
        "truncation marker must be present; got: {observation:?}"
    );
}

#[test]
fn tool_observation_is_bounded_in_role_prompt() {
    // Create an artifact with a file larger than max_observation_bytes (16 KiB).
    let (_temp, view) = {
        let temp = TempDir::new("large-obs");
        let seed = temp.0.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.invalid"]);
        // 20 KiB of content — exceeds the 16 KiB max_observation_bytes default.
        let large = "y".repeat(20 * 1024);
        std::fs::write(seed.join("large.txt"), &large).unwrap();
        git(&seed, &["add", "large.txt"]);
        git(&seed, &["commit", "--quiet", "-m", "add large file"]);
        let bare = temp.0.join("bare.git");
        Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .status()
            .expect("git clone failed");
        let sha = git_rev(&bare);
        (
            temp,
            ArtifactView {
                repo_path: bare,
                commit_sha: sha,
            },
        )
    };

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"read_file","path":"large.txt"}"#,
        r#"{"summary":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_tool_context(producer_request("read the large file"), view),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("[observation truncated]"),
        "large observation must be truncated in the prompt"
    );
    // The tool result section must not contain the full 20 KiB of content.
    let obs_start = second_prompt
        .find("Framework tool observation:")
        .expect("prompt must contain Framework tool observation:");
    let obs_len = second_prompt[obs_start..].len();
    assert!(
        obs_len < 20 * 1024,
        "observation section must be much smaller than 20 KiB; got {obs_len} bytes"
    );
}

#[test]
fn read_only_role_write_request_still_rejected() {
    // Even when the prompt omits write tools, a malicious/confused model
    // that sends a write request must still be rejected by the executor.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"bad.txt","content":"sneaky"}"#,
        r#"{"status":"rejected","reason":"cannot write"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_dummy_tool_context(critic_request("review", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    assert_eq!(requests.len(), 2, "provider must be called twice");
    let second_prompt = &requests[1].prompt;
    assert!(
        second_prompt.contains("not permitted"),
        "executor must reject write even when prompt omits write tools; got:\n{second_prompt}"
    );
    assert!(
        !output.artifact_changed,
        "rejected write must not mark the workspace changed"
    );
}

// ── regression: echoed placeholder tool requests must not execute ───────
//
// A confused model sometimes echoes the tool-section examples verbatim,
// returning {"tool":"replace_text","path":"output.txt","old":"...","new":"..."}
// or {"tool":"write_file","path":"output.txt","content":"..."}.  These must
// be treated as parse failures and trigger a protocol retry, NOT executed as
// real tool calls.  This was the root cause of the "missing field `status`"
// failure observed in the 2026-06-24 run.
