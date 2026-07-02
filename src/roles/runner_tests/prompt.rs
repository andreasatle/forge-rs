use super::*;

const PLAN_RESPONSE: &str = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;

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
