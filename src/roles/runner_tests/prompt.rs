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
fn worker_role_descriptions_render_for_plan_producer_only() {
    // Invariant: the "Available worker roles" section is built from
    // RolePolicy::worker_role_descriptions and appears only in the
    // Plan-node Producer's prompt — Critic, Referee, the Work-node
    // Producer, and the Decomposition-node Producer never assign roles, so
    // they must not see it.
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
            "decomposition producer",
            RoleRequest {
                node_kind: NodeKind::Decomposition,
                ..plan_request("decompose the work")
            },
            PLAN_RESPONSE,
            false,
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
                prompt.contains("Available worker roles:")
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
    prompt.contains("Node review contract")
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
