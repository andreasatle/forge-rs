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
fn reviewer_prompt_explains_tests_planned_separately() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"review ok"}"#]);
    let runner = ProviderRoleRunner::new(&provider);
    let mut request = critic_request("implement fibonacci", "source updated");
    request.context.target_files = vec!["main.py".to_string()];
    request.test_plan_context = TestPlanContext {
        required_test_targets: vec!["test_main.py".to_string()],
        planned_test_targets: vec!["test_main.py".to_string()],
    };

    runner.run_role(request, &crate::telemetry::NoopTelemetry);

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("Node review contract (typed role-boundary metadata)"),
        "critic prompt must include typed node review contract; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Required test targets covered by declared follow-up work: test_main.py"),
        "critic prompt must identify planned downstream test coverage; got:\n{prompt}"
    );
    assert!(
        prompt.contains(
            "Do not reject this current node solely because covered tests are planned separately"
        ),
        "critic prompt must explain the graph-aware exception; got:\n{prompt}"
    );
}

#[test]
fn referee_prompt_explains_missing_planned_test_target_from_metadata() {
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new(&provider);
    let mut request = referee_request(
        "implement fibonacci and mention test_main.py in prose only",
        "source updated",
        "review ok",
    );
    request.context.target_files = vec!["main.py".to_string()];
    request.test_plan_context = TestPlanContext {
        required_test_targets: vec!["test_main.py".to_string()],
        planned_test_targets: vec![],
    };

    runner.run_role(request, &crate::telemetry::NoopTelemetry);

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt
            .contains("Required test targets not covered by declared follow-up work: test_main.py"),
        "referee prompt must use structured planned targets, not objective text; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Declared follow-up/dependent target files: none"),
        "referee prompt must report no planned test target despite objective prose; got:\n{prompt}"
    );
}

#[test]
fn source_only_node_with_planned_test_node_has_consistent_reviewer_contract() {
    use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"review ok"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, CodingProjectAdapter.role_policy());
    let mut request = critic_request(
        "Implement fibonacci(n: int) in main.py.",
        "main.py implements fibonacci correctly",
    );
    request.context.target_files = vec!["main.py".to_string()];
    request.test_plan_context = TestPlanContext {
        required_test_targets: vec!["test_main.py".to_string()],
        planned_test_targets: vec!["test_main.py".to_string()],
    };

    runner.run_role(request, &crate::telemetry::NoopTelemetry);

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("Node review contract (typed role-boundary metadata)")
            && prompt.contains("Declared follow-up/dependent target files: test_main.py"),
        "prompt must distinguish current source deliverable from planned test deliverable; got:\n{prompt}"
    );
    assert!(
        prompt.contains(
            "Acceptance guidance: accept a correct source-only current node even when these covered test files do not exist yet"
        ),
        "source-only node with planned tests must be allowed to accept when implementation is correct; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("Reject code changes that omit corresponding tests"),
        "reviewer system guidance must not duplicate concrete test rejection rules; got:\n{prompt}"
    );
}

#[test]
fn implementation_and_tests_node_can_still_reject_missing_tests() {
    use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"missing tests"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, CodingProjectAdapter.role_policy());
    let mut request = referee_request(
        "Implement fibonacci(n: int) in main.py and add tests in test_main.py.",
        "main.py changed but test_main.py was not created",
        "critic says tests are missing",
    );
    request.context.target_files = vec!["main.py".to_string(), "test_main.py".to_string()];
    request.test_plan_context = TestPlanContext {
        required_test_targets: vec!["test_main.py".to_string()],
        planned_test_targets: vec![],
    };

    runner.run_role(request, &crate::telemetry::NoopTelemetry);

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("Current node target files: main.py, test_main.py"),
        "prompt must preserve implementation target context; got:\n{prompt}"
    );
    assert!(
        prompt.contains("test_main.py"),
        "prompt must preserve test target context; got:\n{prompt}"
    );
    assert!(
        prompt.contains(
            "Acceptance guidance: if this current node changes code and no declared follow-up covers these tests, missing tests remain a valid rejection"
        ),
        "implementation+tests node must still allow rejection for missing tests; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("Reject code changes that omit corresponding tests"),
        "reviewer system prompt must not duplicate missing-test rejection outside the contract; got:\n{prompt}"
    );
}

#[test]
fn reviewer_prompt_has_no_test_guidance_contradiction_when_tests_are_planned() {
    use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, CodingProjectAdapter.role_policy());
    let mut request = referee_request(
        "Implement fibonacci(n: int) in main.py.",
        "main.py implements fibonacci correctly",
        "critic says source is correct",
    );
    request.context.target_files = vec!["main.py".to_string()];
    request.test_plan_context = TestPlanContext {
        required_test_targets: vec!["test_main.py".to_string()],
        planned_test_targets: vec!["test_main.py".to_string()],
    };

    runner.run_role(request, &crate::telemetry::NoopTelemetry);

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains(
            "Do not reject this current node solely because covered tests are planned separately"
        ),
        "prompt must include planned-test allowance; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("Reject code changes that do not include corresponding tests"),
        "prompt must not include old unconditional missing-test rejection; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("Reject code changes that omit corresponding tests"),
        "prompt must not duplicate concrete missing-test rejection outside the contract; got:\n{prompt}"
    );
}

#[test]
fn coding_reviewer_prompts_forbid_rejecting_unstated_preferences() {
    use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"rejected","reason":"reviewed contract issue"}"#,
        r#"{"status":"rejected","reason":"reviewed contract issue"}"#,
    ]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, CodingProjectAdapter.role_policy());

    runner.run_role(
        critic_request(
            "Create a reusable fibonacci(n: int) function.",
            "implemented recursive fibonacci",
        ),
        &crate::telemetry::NoopTelemetry,
    );
    runner.run_role(
        referee_request(
            "Create a reusable fibonacci(n: int) function.",
            "implemented recursive fibonacci",
            "critic review",
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    for (label, prompt) in [
        ("critic", requests[0].prompt.as_str()),
        ("referee", requests[1].prompt.as_str()),
    ] {
        assert!(
            prompt.contains("Ground every rejection in the current node objective"),
            "{label} prompt must ground rejection in explicit contract; got:\n{prompt}"
        );
        assert!(
            prompt.contains(
                "Do not reject solely for unstated preferences about style, algorithm, architecture, or performance"
            ),
            "{label} prompt must forbid rejection on unstated preferences; got:\n{prompt}"
        );
        assert!(
            prompt.contains("mention it in accepted content as advisory only"),
            "{label} prompt must make non-contract preference concerns advisory-only; got:\n{prompt}"
        );
    }
}

#[test]
fn coding_reviewer_prompt_does_not_allow_rejecting_recursive_fibonacci_by_preference() {
    use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, CodingProjectAdapter.role_policy());

    runner.run_role(
        referee_request(
            "Create a reusable fibonacci(n: int) function and unit tests.",
            "main.py implements fibonacci recursively; tests cover base and recursive cases",
            "critic says iterative might be faster",
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("Create a reusable fibonacci(n: int) function")
            && !prompt.contains("must be iterative"),
        "test objective must not itself require iterative implementation; got:\n{prompt}"
    );
    assert!(
        prompt.contains(
            "do not reject recursive code solely because an iterative version might be faster"
        ),
        "referee prompt must not permit rejecting recursive Fibonacci solely for iterative preference; got:\n{prompt}"
    );
}

#[test]
fn coding_reviewer_prompt_preserves_explicit_iterative_requirement() {
    use crate::project::{CodingProjectAdapter, ProjectAdapter as _};

    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"not iterative"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, CodingProjectAdapter.role_policy());

    runner.run_role(
        referee_request(
            "Create fibonacci(n: int) using an iterative implementation for performance.",
            "main.py implements fibonacci recursively",
            "critic says objective requested iterative implementation",
        ),
        &crate::telemetry::NoopTelemetry,
    );

    let prompt = &provider.requests.borrow()[0].prompt;
    assert!(
        prompt.contains("using an iterative implementation for performance"),
        "explicit iterative/performance contract must remain visible in prompt; got:\n{prompt}"
    );
    assert!(
        prompt.contains("unless the contract requires iteration or a performance bound"),
        "reviewer prompt must allow rejection when iteration/performance is explicit; got:\n{prompt}"
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
