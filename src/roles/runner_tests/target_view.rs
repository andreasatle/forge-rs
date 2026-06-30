use super::*;

fn producer_prompt_for_targets(
    view: ArtifactView,
    objective: &str,
    target_files: Vec<String>,
) -> String {
    const BUDGET: usize = 16 * 1024;
    let target_views = crate::project::build_file_text_target_views(&view, &target_files, BUDGET);
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed safely"}"#]);
    let runner = ProviderRoleRunner::new(&provider);
    runner.run_role(
        RoleRequest {
            role: DeliberationRole::Producer,
            objective: objective.to_string(),
            context: crate::machines::deliberation::DeliberationContext {
                target_files,
                ..Default::default()
            },
            test_plan_context: TestPlanContext::default(),
            target_views,
            producer_content: None,
            critic_content: None,
            feedback: vec![],
            node_kind: NodeKind::Work,
            tool_context: Some(RoleToolContext {
                artifact_view: Box::new(view),
                writable_workspace: None,
            }),
        },
        &crate::telemetry::NoopTelemetry,
    );
    provider.requests.borrow()[0].prompt.clone()
}

fn target_state_section(prompt: &str) -> &str {
    let start = prompt
        .find("Target state view")
        .expect("prompt must include a target-state view");
    let end = prompt[start..]
        .find("\nProducer returns")
        .or_else(|| prompt[start..].find("\nCritic accepts"))
        .or_else(|| prompt[start..].find("\nReferee accepts"))
        .or_else(|| prompt[start..].find("\nAvailable file tools:"))
        .map(|offset| start + offset)
        .unwrap_or(prompt.len());
    &prompt[start..end]
}

#[test]
fn existing_target_file_content_appears_in_producer_prompt() {
    let (_temp, view) = make_view("target-state-existing");

    let prompt =
        producer_prompt_for_targets(view, "update the greeting", vec!["hello.txt".to_string()]);
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: hello.txt"),
        "target state must name the structured target; got:\n{section}"
    );
    assert!(
        section.contains("exists: true"),
        "existing target must be marked exists:true; got:\n{section}"
    );
    assert!(
        section.contains("hello world"),
        "existing target text must appear in Producer prompt; got:\n{section}"
    );
}

#[test]
fn missing_target_is_explicitly_marked_absent() {
    let (_temp, view) = make_view("target-state-missing");

    let prompt = producer_prompt_for_targets(
        view,
        "create the missing target",
        vec!["missing.txt".to_string()],
    );
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: missing.txt"),
        "target state must name the missing target; got:\n{section}"
    );
    assert!(
        section.contains("exists: false"),
        "missing target must be marked exists:false; got:\n{section}"
    );
    assert!(
        section.contains("representation: absent"),
        "missing target must use absent representation; got:\n{section}"
    );
}

#[test]
fn target_state_uses_structured_target_files_not_prompt_text() {
    let (_temp, view) = make_view_with_entries(
        "target-state-structured",
        &[
            ("structured.txt", b"structured content\n".as_slice()),
            ("mentioned-only.txt", b"prompt-only content\n".as_slice()),
        ],
    );

    let prompt = producer_prompt_for_targets(
        view,
        "Update mentioned-only.txt, but the structured target is authoritative.",
        vec!["structured.txt".to_string()],
    );
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: structured.txt"),
        "target state must include structured target; got:\n{section}"
    );
    assert!(
        section.contains("structured content"),
        "target state must include structured target content; got:\n{section}"
    );
    assert!(
        !section.contains("mentioned-only.txt") && !section.contains("prompt-only content"),
        "target state must not be inferred from objective wording; got:\n{section}"
    );
}

#[test]
fn prompt_wording_changes_do_not_affect_target_state() {
    let (_temp, view) = make_view("target-state-wording");
    let target_files = vec!["hello.txt".to_string()];

    let first = producer_prompt_for_targets(
        view.clone(),
        "Please edit hello.txt with concise wording.",
        target_files.clone(),
    );
    let second = producer_prompt_for_targets(
        view,
        "Completely different phrasing that still uses the same target.",
        target_files,
    );

    assert_eq!(
        target_state_section(&first),
        target_state_section(&second),
        "target state must depend on structured target_files, not objective wording"
    );
}

#[test]
fn large_and_unreadable_targets_are_represented_safely() {
    let large = vec![b'x'; 16 * 1024 + 1];
    let binary = [0xff, 0xfe, 0xfd, b'\n'];
    let (_temp, view) = make_view_with_entries(
        "target-state-safe-errors",
        &[
            ("large.txt", large.as_slice()),
            ("binary.dat", binary.as_slice()),
        ],
    );

    let prompt = producer_prompt_for_targets(
        view,
        "inspect target state safely",
        vec!["large.txt".to_string(), "binary.dat".to_string()],
    );
    let section = target_state_section(&prompt);

    assert!(
        section.contains("- target: large.txt") && section.contains("too large"),
        "large target must be summarized without full content; got:\n{section}"
    );
    assert!(
        section.contains("- target: binary.dat")
            && section.contains("binary or non-UTF-8 file cannot be represented as text"),
        "unreadable/binary target must be represented as a safe error; got:\n{section}"
    );
}

#[test]
fn prompt_wording_does_not_control_allowed_paths() {
    let (_temp, view) = make_view("prompt-wording-targets");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"prompt.txt","content":"wrong\n"}"#,
        r#"{"tool":"write_file","path":"main.py","content":"right\n"}"#,
        r#"{"status":"accepted","content":"completed"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            with_target_files(
                producer_request("Update the program.\n\nTarget files: prompt.txt"),
                &["main.py"],
            ),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("Target files: main.py"),
        "prompt should render structured targets; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Target files: prompt.txt"),
        "objective wording is still visible as prompt text; got:\n{prompt}"
    );

    assert!(
        output.artifact_changed,
        "structured target write should mark the workspace changed"
    );
}

#[test]
fn structured_target_files_control_tool_permissions_not_objective_text() {
    let (_temp, view) = make_view("structured-target-permissions");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"prompt.txt","content":"wrong\n"}"#,
        r#"{"status":"accepted","content":"completed without writing prompt target"}"#,
    ]);
    let runner = ProviderRoleRunner::new(&provider);

    let output = runner.run_role(
        with_tool_context(
            with_target_files(
                producer_request("Update the program.\n\nTarget files: prompt.txt"),
                &["main.py"],
            ),
            view,
        ),
        &crate::telemetry::NoopTelemetry,
    );

    assert!(
        matches!(output.result, RoleResult::Accepted { .. }),
        "runner must continue after rejecting a prompt-text-only target; got {:?}",
        output.result
    );
    assert!(
        !output.artifact_changed,
        "tool permissions must reject writes to targets named only in objective text"
    );
    let retry_prompt = &provider.requests.borrow()[1].prompt;
    assert!(
        retry_prompt.contains("prompt.txt"),
        "error observation should identify the rejected path; got:\n{retry_prompt}"
    );
    assert!(
        retry_prompt.contains("main.py"),
        "retry context should preserve the structured target path; got:\n{retry_prompt}"
    );
}
