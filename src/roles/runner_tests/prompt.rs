use super::*;

const PLAN_RESPONSE: &str = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
const PLAN_RESPONSE_WITH_ROLE: &str = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","role":"implementer","targets":["work.txt"],"depends_on":[]}]}"#;

#[test]
fn rendered_prompts_use_expected_role_schemas() {
    let cases = [
        (
            "planner producer",
            plan_request("plan the work"),
            PLAN_RESPONSE,
            &["`tasks`"][..],
            &["`summary`", "`status`"][..],
        ),
        (
            "worker producer",
            producer_request("do the work"),
            r#"{"summary":"work done"}"#,
            &["`summary`"][..],
            &["`tasks`", "`status`", "`content`", "`reason`"][..],
        ),
        (
            "critic",
            critic_request("review the draft", "draft"),
            r#"{"status":"rejected","reason":"needs work"}"#,
            &["`status`", "`content`", "`reason`"][..],
            &["`tasks`", "`summary`"][..],
        ),
        (
            "referee",
            referee_request("approve the result", "draft", "review"),
            r#"{"status":"rejected","reason":"not ready"}"#,
            &["`status`", "`content`", "`reason`"][..],
            &["`tasks`", "`summary`"][..],
        ),
    ];

    for (label, request, response, required, forbidden) in cases {
        let prompt = first_prompt(request, response);
        assert_fields(label, &prompt, required, forbidden);
    }
}

#[test]
fn rendered_prompts_have_expected_tool_visibility() {
    let cases = [
        (
            "planner",
            plan_request("plan the work"),
            PLAN_RESPONSE,
            &[][..],
            &[
                "list_files",
                "read_file",
                "write_file",
                "replace_text",
                "delete_file",
            ][..],
        ),
        (
            "producer",
            with_dummy_tool_context(producer_request("do the work")),
            r#"{"summary":"work done"}"#,
            &[
                "list_files",
                "read_file",
                "write_file",
                "replace_text",
                "delete_file",
            ][..],
            &[][..],
        ),
        (
            "critic",
            with_dummy_tool_context(critic_request("review the draft", "draft")),
            r#"{"status":"rejected","reason":"needs work"}"#,
            &["list_files", "read_file"][..],
            &["write_file", "replace_text", "delete_file"][..],
        ),
        (
            "referee",
            with_dummy_tool_context(referee_request("approve the result", "draft", "review")),
            r#"{"status":"rejected","reason":"not ready"}"#,
            &["list_files", "read_file"][..],
            &["write_file", "replace_text", "delete_file"][..],
        ),
    ];

    for (label, request, response, required, forbidden) in cases {
        let prompt = first_prompt(request, response);
        assert_fields(label, &prompt, required, forbidden);
    }
}

#[test]
fn review_contract_renders_for_reviewers_only() {
    let mut producer = producer_request("do the work");
    producer.context.target_files = vec!["main.py".to_string()];
    producer.test_plan_context = test_plan_context();

    let mut critic = critic_request("review the draft", "draft");
    critic.context.target_files = vec!["main.py".to_string()];
    critic.test_plan_context = test_plan_context();

    let mut referee = referee_request("approve the result", "draft", "review");
    referee.context.target_files = vec!["main.py".to_string()];
    referee.test_plan_context = test_plan_context();

    let producer_prompt = first_prompt(producer, r#"{"summary":"work done"}"#);
    let critic_prompt = first_prompt(critic, r#"{"status":"rejected","reason":"needs work"}"#);
    let referee_prompt = first_prompt(referee, r#"{"status":"rejected","reason":"not ready"}"#);

    assert!(!has_review_contract(&producer_prompt));
    assert!(has_review_contract(&critic_prompt));
    assert!(has_review_contract(&referee_prompt));
}

#[test]
fn split_prior_attempt_context_renders_in_its_own_section_not_inside_objective() {
    // Reconstructs the scenario a Split-recovered Plan node produces: an
    // ambiguous objective ("implement fibonacci") plus diagnostic context
    // from a Referee's prior rejection. Before the objective/prior-attempt
    // split, `apply_split` baked the rejection text directly into the
    // objective string, so every role — including a Referee reviewing a
    // Critic-accepted revision in the same round — saw the old rejection
    // rendered as if it were part of the task requirement itself. This test
    // proves the rendered prompt no longer conflates the two: the objective
    // section holds only the original task, and the prior-attempt text
    // appears solely in its own separately labeled section.
    const REJECTION_TEXT: &str = "Referee rejected: the interface for computing fibonacci \
                                   numbers is ambiguous — unclear whether it should return a \
                                   single value, a sequence, or accept memoization.";

    let mut plan_producer = plan_request("implement fibonacci");
    plan_producer.context.prior_attempt_context = Some(REJECTION_TEXT.to_string());

    let mut referee = referee_request("implement fibonacci", "draft plan", "revised review");
    referee.node_kind = NodeKind::Plan;
    referee.context.prior_attempt_context = Some(REJECTION_TEXT.to_string());

    for (label, request, response) in [
        ("plan producer", plan_producer, PLAN_RESPONSE),
        (
            "plan referee",
            referee,
            r#"{"status":"accepted","content":"looks good"}"#,
        ),
    ] {
        let prompt = first_prompt(request, response);

        // The objective section is exactly the original task — nothing else
        // is appended to it.
        assert!(
            prompt.contains("# Objective\nimplement fibonacci\n\n"),
            "[{label}] objective section must contain only the original task; got:\n{prompt}"
        );

        // The prior-attempt context appears in its own clearly-labeled
        // section, separate from "# Objective".
        assert!(
            prompt.contains("# Previous Attempt"),
            "[{label}] must render a distinct Previous Attempt section; got:\n{prompt}"
        );
        assert!(
            prompt.contains(REJECTION_TEXT),
            "[{label}] Previous Attempt section must carry the diagnostic text; got:\n{prompt}"
        );

        // The two sections must not be merged into one block.
        let objective_section = prompt
            .split("\n\n")
            .find(|part| part.starts_with("# Objective"))
            .unwrap_or_else(|| panic!("[{label}] no # Objective section found in:\n{prompt}"));
        assert!(
            !objective_section.contains("Referee rejected"),
            "[{label}] Previous Attempt text must not leak into the Objective section; got:\n{objective_section}"
        );
    }
}

#[test]
fn missing_required_test_target_is_never_framed_as_grounds_for_rejection() {
    // Invariant: source-producing and test-producing work are authored in
    // parallel by independent teams (e.g. `implement` and `create_test`),
    // not sequenced within one team's own plan graph. A referee/critic
    // reviewing a source-only node therefore always sees the adapter's
    // required test target as "not covered by declared follow-up work" —
    // that sibling team's node lives in a different graph entirely — and
    // this must never be rendered as valid grounds for rejecting the node.
    // Regression for a prompt that told the model "missing tests remain a
    // valid rejection" whenever no in-graph follow-up node covered the
    // required test target, which caused referees to reject correct source
    // changes solely because a sibling team hadn't finished yet.
    let uncovered_context = TestPlanContext {
        required_validation_targets: vec!["tests/test_fibonacci.py".to_string()],
        planned_test_targets: vec![],
    };

    let mut referee = referee_request("Implement a Fibonacci number generator", "draft", "review");
    referee.context.target_files = vec!["src/fibonacci.py".to_string()];
    referee.test_plan_context = uncovered_context;

    let prompt = first_prompt(referee, r#"{"status":"accepted","content":"looks good"}"#);

    assert!(
        prompt.contains("tests/test_fibonacci.py"),
        "prompt must still surface the required test target; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("valid rejection"),
        "prompt must never frame a missing test target as valid rejection grounds; got:\n{prompt}"
    );
    assert!(
        prompt.contains("never grounds for rejecting this node"),
        "prompt must explicitly say a missing test file is never grounds for rejection; got:\n{prompt}"
    );
}

#[test]
fn worker_role_descriptions_render_for_plan_producer_only() {
    // Invariant: the "Available worker roles" section is built from
    // RolePolicy::worker_role_descriptions and appears only in the
    // Plan-node Producer's prompt — Critic, Referee, and the Work-node
    // Producer never assign roles, so they must not see it.
    let policy = RolePolicy {
        worker_role_descriptions: vec![
            ("tester".to_string(), "Writes test files.".to_string()),
            ("implementer".to_string(), "Writes source code.".to_string()),
        ],
        ..RolePolicy::default()
    };

    let cases = [
        (
            "plan producer",
            plan_request("plan the work"),
            PLAN_RESPONSE_WITH_ROLE,
            true,
        ),
        (
            "worker producer",
            producer_request("do the work"),
            r#"{"summary":"work done"}"#,
            false,
        ),
        (
            "critic",
            critic_request("review the draft", "draft"),
            r#"{"status":"rejected","reason":"needs work"}"#,
            false,
        ),
        (
            "referee",
            referee_request("approve the result", "draft", "review"),
            r#"{"status":"rejected","reason":"not ready"}"#,
            false,
        ),
    ];

    for (label, request, response, expects_worker_roles) in cases {
        let provider = ScriptedProvider::from_strs(&[response]);
        let runner = ProviderRoleRunner::new_with_policy(&provider, policy.clone());
        runner.run_role(request, &crate::telemetry::NoopTelemetry);
        let prompt = provider.requests.borrow()[0].prompt.clone();
        if expects_worker_roles {
            assert!(
                prompt.contains("# Available Worker Roles")
                    && prompt.contains("- tester: Writes test files.")
                    && prompt.contains("- implementer: Writes source code."),
                "{label} prompt must list worker role descriptions; got:\n{prompt}"
            );
        } else {
            assert!(
                !prompt.contains("Available worker roles:"),
                "{label} prompt must not list worker role descriptions; got:\n{prompt}"
            );
        }
    }
}

#[test]
fn work_node_producer_uses_matching_worker_role_prompt() {
    // Invariant: a Work-node role whose worker_role matches an entry in
    // RolePolicy::worker_role_policies is rendered with that entry's prompt,
    // not the shared worker_producer_system field.
    let policy = RolePolicy {
        worker_producer_system: "SHARED PRODUCER MARKER".to_string(),
        worker_role_policies: [(
            "tester".to_string(),
            crate::roles::policy::WorkerRolePolicy {
                producer_system: "TESTER PRODUCER MARKER".to_string(),
                critic_system: "TESTER CRITIC MARKER".to_string(),
                referee_system: "TESTER REFEREE MARKER".to_string(),
            },
        )]
        .into_iter()
        .collect(),
        ..RolePolicy::default()
    };

    let mut request = producer_request("do the work");
    request.worker_role = Some("tester".to_string());

    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"work done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);
    runner.run_role(request, &crate::telemetry::NoopTelemetry);
    let prompt = provider.requests.borrow()[0].prompt.clone();

    assert!(
        prompt.contains("TESTER PRODUCER MARKER"),
        "expected the tester role's own prompt; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("SHARED PRODUCER MARKER"),
        "shared worker prompt must not be used when a matching role policy exists; got:\n{prompt}"
    );
}

#[test]
fn work_node_falls_back_to_shared_prompt_when_role_unset_or_unmatched() {
    // Invariant: a Work node with no worker_role, or one absent from
    // worker_role_policies, still uses the shared worker_*_system fields —
    // per-role dispatch must not change behavior for adapters with no
    // configured worker roles, or nodes the planner left unassigned.
    let policy = RolePolicy {
        worker_producer_system: "SHARED PRODUCER MARKER".to_string(),
        worker_role_policies: [(
            "tester".to_string(),
            crate::roles::policy::WorkerRolePolicy {
                producer_system: "TESTER PRODUCER MARKER".to_string(),
                critic_system: "TESTER CRITIC MARKER".to_string(),
                referee_system: "TESTER REFEREE MARKER".to_string(),
            },
        )]
        .into_iter()
        .collect(),
        ..RolePolicy::default()
    };

    for worker_role in [None, Some("implementer".to_string())] {
        let mut request = producer_request("do the work");
        request.worker_role = worker_role.clone();

        let provider = ScriptedProvider::from_strs(&[r#"{"summary":"work done"}"#]);
        let runner = ProviderRoleRunner::new_with_policy(&provider, policy.clone());
        runner.run_role(request, &crate::telemetry::NoopTelemetry);
        let prompt = provider.requests.borrow()[0].prompt.clone();

        assert!(
            prompt.contains("SHARED PRODUCER MARKER"),
            "worker_role {worker_role:?} must fall back to the shared prompt; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("TESTER PRODUCER MARKER"),
            "worker_role {worker_role:?} must not pick up an unrelated role's prompt; got:\n{prompt}"
        );
    }
}

fn first_prompt(request: RoleRequest, response: &str) -> String {
    let provider = ScriptedProvider::from_strs(&[response]);
    let runner = ProviderRoleRunner::new(&provider);
    runner.run_role(request, &crate::telemetry::NoopTelemetry);
    provider.requests.borrow()[0].prompt.clone()
}

fn test_plan_context() -> TestPlanContext {
    TestPlanContext {
        required_validation_targets: vec!["test_main.py".to_string()],
        planned_test_targets: vec!["test_main.py".to_string()],
    }
}

fn has_review_contract(prompt: &str) -> bool {
    prompt.contains("# Node Review Contract")
}

fn assert_fields(label: &str, prompt: &str, required: &[&str], forbidden: &[&str]) {
    for field in required {
        assert!(
            prompt.contains(field),
            "{label} prompt is missing {field}; got:\n{prompt}"
        );
    }
    for field in forbidden {
        assert!(
            !prompt.contains(field),
            "{label} prompt includes unexpected {field}; got:\n{prompt}"
        );
    }
}
