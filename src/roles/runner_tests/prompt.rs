use super::*;

#[test]
fn role_prompt_includes_feedback() {
    let feedback = vec![RevisionFeedback {
        reason: "too vague".to_string(),
    }];
    let default = RolePolicy::default();
    let prompt = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "write a poem",
        None,
        None,
        &feedback,
        &[],
    );
    assert!(
        prompt.contains("too vague"),
        "expected prompt to include feedback reason 'too vague', got: {prompt}"
    );
    assert!(
        prompt.contains("write a poem"),
        "expected prompt to include objective, got: {prompt}"
    );
    assert!(
        prompt.contains("\"status\""),
        "expected prompt to include JSON schema instructions, got: {prompt}"
    );
}

#[test]
fn role_prompt_includes_tool_request_as_valid_response_when_tools_available() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("test with tools")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("tool request"),
        "prompt must describe tool request as a valid response when tools are available"
    );
    assert!(
        prompt.contains("list_files"),
        "prompt must include example tool requests"
    );
}

#[test]
fn role_prompt_has_single_protocol_wrapper() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(producer_request("test"), &crate::telemetry::NoopTelemetry);

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    // "Accepted schema:" is the old InstructedProvider outer wrapper text.
    // render_role_prompt uses "Accepted:" (without "schema").
    assert!(
        !prompt.contains("Accepted schema:"),
        "prompt must not contain InstructedProvider outer wrapper text"
    );
    assert!(
        prompt.contains("\"status\""),
        "prompt must still contain the role protocol instructions"
    );
}

#[test]
fn tool_prompt_matches_policy() {
    let rw_policy = FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    };
    let ro_policy = FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    };

    let rw_section = super::render_tool_section(&rw_policy);
    let ro_section = super::render_tool_section(&ro_policy);

    assert!(
        rw_section.contains("write_file"),
        "allow_writes=true must render write_file"
    );
    assert!(
        rw_section.contains("replace_text"),
        "allow_writes=true must render replace_text"
    );
    assert!(
        rw_section.contains("delete_file"),
        "allow_writes=true must render delete_file"
    );
    assert!(
        !ro_section.contains("write_file"),
        "allow_writes=false must not render write_file"
    );
    assert!(
        !ro_section.contains("replace_text"),
        "allow_writes=false must not render replace_text"
    );
    assert!(
        !ro_section.contains("delete_file"),
        "allow_writes=false must not render delete_file"
    );
    assert!(
        ro_section.contains("list_files"),
        "allow_writes=false must still render list_files"
    );
    assert!(
        ro_section.contains("read_file"),
        "allow_writes=false must still render read_file"
    );
}

#[test]
fn tool_prompt_for_target_main_py_shows_exact_read_file_path() {
    let policy = FileToolPolicy {
        allowed_paths: Some(vec!["main.py".to_string()]),
        ..FileToolPolicy::default()
    };

    let section = super::render_tool_section(&policy);

    assert!(
        section.contains(r#"{"tool":"read_file","path":"main.py"}"#),
        "target-aware tool section must show exact read_file path; got:\n{section}"
    );
    assert!(
        !section.contains(r#"{"tool":"read_file","path":"path/to/file.txt"}"#),
        "target-aware tool section must not show generic read_file placeholder; got:\n{section}"
    );
}

#[test]
fn work_role_prompt_uses_structured_tool_targets() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(with_target_files(
            producer_request("Update the program."),
            &["main.py"],
        )),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains(r#"{"tool":"read_file","path":"main.py"}"#),
        "Work prompt must render declared target in read_file example; got:\n{prompt}"
    );
    assert!(
        prompt.contains(r#"{"tool":"write_file","path":"main.py""#),
        "Work prompt must render declared target in write_file example; got:\n{prompt}"
    );
}

#[test]
fn planner_prompt_omits_tool_section() {
    // When node_kind is Plan and tool_context is None, no tool section appears.
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("list_files"),
        "planner prompt must not include tool section; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("write_file"),
        "planner prompt must not include write tools; got:\n{prompt}"
    );
}

#[test]
fn worker_prompt_still_has_write_tools() {
    // Work nodes with tool_context keep write tools (existing behaviour preserved).
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("implement the feature")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("write_file"),
        "worker prompt must still include write_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("replace_text"),
        "worker prompt must still include replace_text; got:\n{prompt}"
    );
}

// ── Step 2: planner content validation ───────────────────────────────────

#[test]
fn producer_prompt_lists_write_tools() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(producer_request("produce something")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("write_file"),
        "producer prompt must include write_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("replace_text"),
        "producer prompt must include replace_text; got:\n{prompt}"
    );
    assert!(
        prompt.contains("delete_file"),
        "producer prompt must include delete_file; got:\n{prompt}"
    );
}

#[test]
fn critic_prompt_omits_write_tools() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs work"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(critic_request("review the draft", "draft")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("write_file"),
        "critic prompt must not include write_file; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("replace_text"),
        "critic prompt must not include replace_text; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("delete_file"),
        "critic prompt must not include delete_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("list_files"),
        "critic prompt must include list_files; got:\n{prompt}"
    );
    assert!(
        prompt.contains("read_file"),
        "critic prompt must include read_file; got:\n{prompt}"
    );
}

#[test]
fn referee_prompt_omits_write_tools() {
    // Use a rejection response so the read-file enforcement does not fire
    // (enforcement only applies when the reviewer accepts).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"rejected","reason":"content does not meet requirements"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        with_dummy_tool_context(referee_request(
            "approve the result",
            "content",
            "looks good",
        )),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("write_file"),
        "referee prompt must not include write_file; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("replace_text"),
        "referee prompt must not include replace_text; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("delete_file"),
        "referee prompt must not include delete_file; got:\n{prompt}"
    );
    assert!(
        prompt.contains("list_files"),
        "referee prompt must include list_files; got:\n{prompt}"
    );
    assert!(
        prompt.contains("read_file"),
        "referee prompt must include read_file; got:\n{prompt}"
    );
}
