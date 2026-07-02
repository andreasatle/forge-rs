use super::*;

#[test]
fn no_runtime_prompt_contains_dot_placeholder_json_values() {
    // Render every prompt variant and assert none contains the "..." sentinel
    // as a JSON string value.  "..." in a JSON value is a known trigger for
    // model placeholder-copying (see 2026-06-24 incident).
    let no_dot = |label: &str, prompt: &str| {
        assert!(
            !prompt.contains("\"...\""),
            "{label} must not contain '...' as a JSON value; got:\n{prompt}"
        );
    };

    // Role prompts for all three roles, with and without prior content.
    let default = RolePolicy::default();
    for (role, system, pc, cc) in [
        (
            DeliberationRole::Producer,
            default.worker_producer_system.as_str(),
            None,
            None,
        ),
        (
            DeliberationRole::Critic,
            default.worker_critic_system.as_str(),
            Some("draft"),
            None,
        ),
        (
            DeliberationRole::Referee,
            default.worker_referee_system.as_str(),
            Some("draft"),
            Some("looks good"),
        ),
    ] {
        let prompt =
            render_role_prompt(system, &role, "write a haiku about Rust", pc, cc, &[], &[]);
        no_dot(&format!("{role:?} role prompt"), &prompt);
    }

    // Tool section — write-enabled and read-only.
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    let ro = render_tool_section(&FileToolPolicy {
        allow_writes: false,
        ..FileToolPolicy::default()
    });
    no_dot("write-enabled tool section", &rw);
    no_dot("read-only tool section", &ro);

    // Retry prompt (wraps the base role prompt).
    let base = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "write a haiku",
        None,
        None,
        &[],
        &[],
    );
    let retry = render_retry_prompt(&base, "no JSON object found in role response", true);
    no_dot("retry prompt", &retry);
}

#[test]
fn producer_prompt_describes_tool_request_variants() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    assert!(
        rw.contains("ToolRequest variants"),
        "tool section must describe tool request variants; got:\n{rw}"
    );
    assert!(
        rw.contains("write_file") && rw.contains("complete file content string"),
        "write-enabled section must describe write_file fields; got:\n{rw}"
    );
    assert!(
        rw.contains("replace_text") && rw.contains("exact existing text"),
        "write-enabled section must describe replace_text fields; got:\n{rw}"
    );
    assert!(
        !rw.contains('$') && !rw.contains("\"...\""),
        "tool section must not contain placeholder values; got:\n{rw}"
    );
}

#[test]
fn producer_prompt_distinguishes_write_file_from_replace_text() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });

    assert!(
        rw.contains("Use write_file by default")
            && rw.contains("creating a file")
            && rw.contains("replacing most or all of an existing file"),
        "producer prompt must make write_file the default for creates and whole-file rewrites; got:\n{rw}"
    );
    assert!(
        rw.contains("Use replace_text only for small, localized edits")
            && rw.contains("after you have read the file")
            && rw.contains("exact old string that occurs once"),
        "producer prompt must limit replace_text to exact localized edits after reading; got:\n{rw}"
    );
    assert!(
        rw.contains("whitespace, indentation, or formatting differences will cause it to fail"),
        "producer prompt must explain exact replace_text matching failure modes; got:\n{rw}"
    );
}

#[test]
fn public_file_tool_docs_match_prompt_guidance() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    let readme = include_str!("../../../README.md");

    for phrase in [
        "default tool when replacing most or all of a file",
        "small, localized edit",
        "exact, unique old string",
        "whitespace, indentation, or formatting differences",
        "use `write_file` instead of retrying `replace_text`",
    ] {
        assert!(
            readme.contains(phrase),
            "README must keep file tool guidance consistent; missing phrase: {phrase}"
        );
    }
    assert!(
        rw.contains("replacing most or all of an existing file")
            && readme.contains("replacing most or all of a file"),
        "prompt and README must both describe write_file as the whole-file rewrite tool"
    );
}

#[test]
fn role_response_examples_do_not_use_dot_placeholders() {
    let default = RolePolicy::default();
    for (role, system, pc, cc) in [
        (
            DeliberationRole::Producer,
            default.worker_producer_system.as_str(),
            None,
            None,
        ),
        (
            DeliberationRole::Critic,
            default.worker_critic_system.as_str(),
            Some("draft"),
            None,
        ),
        (
            DeliberationRole::Referee,
            default.worker_referee_system.as_str(),
            Some("draft"),
            Some("looks good"),
        ),
    ] {
        let prompt = render_role_prompt(system, &role, "test objective", pc, cc, &[], &[]);
        assert!(
            !prompt.contains("\"content\":\"...\""),
            "{role:?} prompt must not use '...' for accepted content; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("\"reason\":\"...\""),
            "{role:?} prompt must not use '...' for rejected reason; got:\n{prompt}"
        );
    }
    // Retry prompt schema examples also must not use "...".
    let base = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "test",
        None,
        None,
        &[],
        &[],
    );
    let retry = render_retry_prompt(&base, "parse error", true);
    assert!(
        !retry.contains("\"content\":\"...\""),
        "retry prompt must not use '...' for accepted content; got:\n{retry}"
    );
    assert!(
        !retry.contains("\"reason\":\"...\""),
        "retry prompt must not use '...' for rejected reason; got:\n{retry}"
    );
}

#[test]
fn prompt_schema_uses_descriptions_instead_of_placeholder_examples() {
    let default = RolePolicy::default();
    let base = render_role_prompt(
        &default.worker_producer_system,
        &DeliberationRole::Producer,
        "write a haiku",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        base.contains("Accepted: `status` must be \"accepted\""),
        "role prompt must describe accepted response; got:\n{base}"
    );
    assert!(
        !base.contains('$'),
        "role prompt must not contain placeholders; got:\n{base}"
    );

    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    assert!(
        rw.contains("ToolRequest variants"),
        "write-enabled tool section must describe tool requests; got:\n{rw}"
    );
    assert!(
        !rw.contains('$'),
        "tool section must not contain placeholders; got:\n{rw}"
    );

    let retry = render_retry_prompt(&base, "parse error", true);
    assert!(
        retry.contains("Accepted: `status` must be \"accepted\""),
        "retry prompt must describe accepted response; got:\n{retry}"
    );
    assert!(
        !retry.contains('$'),
        "retry prompt must not contain placeholders; got:\n{retry}"
    );
}

#[test]
fn planner_prompt_shows_direct_planner_output_schema() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("`tasks`"),
        "planner prompt must show direct tasks schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("`id`"),
        "planner prompt must show id field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("`objective`"),
        "planner prompt must show objective field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("`targets`"),
        "planner prompt must show targets field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("`depends_on`"),
        "planner prompt must show depends_on field; got:\n{prompt}"
    );
}

#[test]
fn planner_prompt_does_not_show_status_content_schema() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("\"status\""),
        "planner prompt must not show status/content wrapper; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("$RESPONSE_SUMMARY"),
        "planner prompt must not show accepted placeholder; got:\n{prompt}"
    );
}

#[test]
fn worker_producer_uses_accepted_only_schema() {
    // The Work-node Producer implements; it never rejects. Only Critic and
    // Referee may reject, so the rejected schema must never reach the Producer.
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        producer_request("write some code"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("`status`"),
        "worker prompt must still contain status/content schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Accepted: `status` must be \"accepted\""),
        "worker prompt must describe accepted schema; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("Rejected: `status` must be \"rejected\""),
        "Work-node Producer prompt must never offer the rejected schema; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"rejected\""),
        "Work-node Producer prompt must never mention the rejected status; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"tasks\""),
        "worker prompt must not contain the planner tasks schema; got:\n{prompt}"
    );
}

#[test]
fn critic_still_uses_status_content_schema() {
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"looks good"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        critic_request("review the draft", "some draft"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("`status`"),
        "critic prompt must still contain status/content schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Accepted: `status` must be \"accepted\""),
        "critic prompt must describe accepted schema; got:\n{prompt}"
    );
}

#[test]
fn referee_still_uses_status_content_schema() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(
        referee_request("approve the result", "content", "review"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("`status`"),
        "referee prompt must still contain status/content schema; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Accepted: `status` must be \"accepted\""),
        "referee prompt must describe accepted schema; got:\n{prompt}"
    );
}

// ── tool observation protocol: anti-echo hardening ───────────────────────

#[test]
fn write_tool_schema_does_not_use_output_txt() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    assert!(
        !rw.contains("output.txt"),
        "write_file schema must not use 'output.txt' as the path; got:\n{rw}"
    );
}

#[test]
fn write_tool_schema_does_not_use_hello_world() {
    let rw = render_tool_section(&FileToolPolicy {
        allow_writes: true,
        ..FileToolPolicy::default()
    });
    assert!(
        !rw.contains("Hello, world!"),
        "write_file schema must not use 'Hello, world!' as the content; got:\n{rw}"
    );
}

// ── Decision pressure tests ──────────────────────────────────────────────
