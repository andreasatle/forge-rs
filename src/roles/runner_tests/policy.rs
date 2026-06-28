use super::*;

#[test]
fn default_role_policy_matches_current_prompt_behavior() {
    let policy = RolePolicy::default();
    let prompt = render_role_prompt(
        &policy.worker_producer_system,
        &DeliberationRole::Producer,
        "write a haiku",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("\"status\""),
        "default policy must include JSON status field; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Do not copy example values"),
        "default policy must include copy-guard instruction; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Producer returns accepted content"),
        "default policy must describe Producer role; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Critic accepts"),
        "default policy must describe Critic role; got:\n{prompt}"
    );
    assert!(
        prompt.contains("Referee accepts"),
        "default policy must describe Referee role; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"...\""),
        "default policy must not contain dot-placeholder JSON values; got:\n{prompt}"
    );
}

#[test]
fn planner_prompt_uses_planner_policy() {
    let policy = RolePolicy {
        planner_producer_system: "PLANNER_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.planner_producer_system,
        &DeliberationRole::Producer,
        "plan the work",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("PLANNER_MARKER_XYZ"),
        "planner prompt must include planner_producer_system text; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_MARKER"),
        "planner prompt must not include worker_producer_system text"
    );
}

#[test]
fn worker_prompt_uses_worker_policy() {
    let policy = RolePolicy {
        worker_producer_system: "WORKER_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.worker_producer_system,
        &DeliberationRole::Producer,
        "do the work",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("WORKER_MARKER_XYZ"),
        "worker prompt must include worker_producer_system text; got:\n{prompt}"
    );
}

#[test]
fn critic_prompt_uses_critic_policy() {
    let policy = RolePolicy {
        worker_critic_system: "CRITIC_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.worker_critic_system,
        &DeliberationRole::Critic,
        "review the draft",
        Some("producer draft"),
        None,
        &[],
        &[],
    );
    assert!(
        prompt.contains("CRITIC_MARKER_XYZ"),
        "critic prompt must include worker_critic_system text; got:\n{prompt}"
    );
}

#[test]
fn referee_prompt_uses_referee_policy() {
    let policy = RolePolicy {
        worker_referee_system: "REFEREE_MARKER_XYZ".to_string(),
        ..RolePolicy::default()
    };
    let prompt = render_role_prompt(
        &policy.worker_referee_system,
        &DeliberationRole::Referee,
        "approve the result",
        Some("producer draft"),
        Some("critic review"),
        &[],
        &[],
    );
    assert!(
        prompt.contains("REFEREE_MARKER_XYZ"),
        "referee prompt must include worker_referee_system text; got:\n{prompt}"
    );
}

#[test]
fn default_policy_keeps_json_protocol_instructions() {
    let policy = RolePolicy::default();
    // Worker, Critic, Referee use the status/content wrapper schema.
    for (label, system) in [
        ("worker", policy.worker_producer_system.as_str()),
        ("critic", policy.worker_critic_system.as_str()),
        ("referee", policy.worker_referee_system.as_str()),
    ] {
        let prompt = render_role_prompt(
            system,
            &DeliberationRole::Producer,
            "test",
            None,
            None,
            &[],
            &[],
        );
        assert!(
            prompt.contains("Return exactly one JSON object"),
            "{label} default policy must include JSON-only instruction; got:\n{prompt}"
        );
        assert!(
            prompt.contains("$RESPONSE_SUMMARY"),
            "{label} default policy must include accepted schema placeholder; got:\n{prompt}"
        );
        assert!(
            prompt.contains("$REASON_FOR_REJECTION"),
            "{label} default policy must include rejected schema placeholder; got:\n{prompt}"
        );
    }
    // Planner uses direct PlannerOutput schema — no status/content wrapper.
    let planner_prompt = render_role_prompt(
        &policy.planner_producer_system,
        &DeliberationRole::Producer,
        "test",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        planner_prompt.contains("Return exactly one JSON object"),
        "planner default policy must include JSON-only instruction; got:\n{planner_prompt}"
    );
    assert!(
        planner_prompt.contains("\"tasks\""),
        "planner default policy must include direct tasks schema; got:\n{planner_prompt}"
    );
    assert!(
        !planner_prompt.contains("$RESPONSE_SUMMARY"),
        "planner default policy must not include status/content placeholder; got:\n{planner_prompt}"
    );
}

#[test]
fn role_policy_does_not_change_tool_visibility() {
    // Tool visibility is controlled by FileToolPolicy (file_tool_policy_for_role),
    // not by RolePolicy. Verify that changing system text has no effect.
    let policy = RolePolicy {
        worker_producer_system: "CUSTOM_WORKER".to_string(),
        worker_critic_system: "CUSTOM_CRITIC".to_string(),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        with_dummy_tool_context(producer_request("produce something")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("write_file"),
        "producer must still see write tools regardless of custom policy; got:\n{prompt}"
    );
    assert!(
        prompt.contains("CUSTOM_WORKER"),
        "custom worker_producer_system must appear in producer prompt; got:\n{prompt}"
    );
}

// ── NodeKind policy routing ───────────────────────────────────────────────

#[test]
fn planner_node_uses_planner_policy() {
    let policy = RolePolicy {
        planner_producer_system: "PLANNER_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[tasks_json]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("PLANNER_MARKER"),
        "plan node must use planner_producer_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_MARKER"),
        "plan node must not use worker_producer_system"
    );
}

#[test]
fn work_node_uses_worker_policy() {
    let policy = RolePolicy {
        worker_producer_system: "WORKER_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"work done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        producer_request("do the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("WORKER_MARKER"),
        "work node must use worker_producer_system; got:\n{prompt}"
    );
}

#[test]
fn plan_critic_uses_planner_critic_policy() {
    let policy = RolePolicy {
        planner_critic_system: "PLANNER_CRITIC_MARKER".to_string(),
        worker_critic_system: "WORKER_CRITIC_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"plan review done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        RoleRequest {
            node_kind: NodeKind::Plan,
            ..critic_request("review the plan", "plan graph")
        },
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("PLANNER_CRITIC_MARKER"),
        "plan critic must use planner_critic_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_CRITIC_MARKER"),
        "plan critic must not use worker_critic_system; got:\n{prompt}"
    );
}

#[test]
fn work_critic_uses_worker_critic_policy() {
    let policy = RolePolicy {
        planner_critic_system: "PLANNER_CRITIC_MARKER".to_string(),
        worker_critic_system: "WORKER_CRITIC_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"review done"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        critic_request("review the draft", "draft"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("WORKER_CRITIC_MARKER"),
        "work critic must use worker_critic_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("PLANNER_CRITIC_MARKER"),
        "work critic must not use planner_critic_system; got:\n{prompt}"
    );
}

#[test]
fn plan_referee_uses_planner_referee_policy() {
    let policy = RolePolicy {
        planner_referee_system: "PLANNER_REFEREE_MARKER".to_string(),
        worker_referee_system: "WORKER_REFEREE_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider =
        ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"plan approved"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        RoleRequest {
            node_kind: NodeKind::Plan,
            ..referee_request("approve the plan", "plan graph", "plan review")
        },
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("PLANNER_REFEREE_MARKER"),
        "plan referee must use planner_referee_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("WORKER_REFEREE_MARKER"),
        "plan referee must not use worker_referee_system; got:\n{prompt}"
    );
}

#[test]
fn work_referee_uses_worker_referee_policy() {
    let policy = RolePolicy {
        planner_referee_system: "PLANNER_REFEREE_MARKER".to_string(),
        worker_referee_system: "WORKER_REFEREE_MARKER".to_string(),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"approved"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        referee_request("approve the result", "content", "review"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("WORKER_REFEREE_MARKER"),
        "work referee must use worker_referee_system; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("PLANNER_REFEREE_MARKER"),
        "work referee must not use planner_referee_system; got:\n{prompt}"
    );
}

#[test]
fn default_policy_preserves_existing_behavior() {
    let policy = RolePolicy::default();
    let tasks_json = r#"{"tasks":[{"id":"t1","objective":"do the work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"work done"}"#,
    ]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        plan_request("plan the work"),
        &crate::telemetry::NoopTelemetry,
    );
    runner.run_role(
        producer_request("do the work"),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    for (label, req) in [("plan", &requests[0]), ("work", &requests[1])] {
        assert!(
            req.prompt.contains("Return exactly one JSON object"),
            "{label} producer prompt must contain JSON protocol instructions; got:\n{}",
            req.prompt
        );
    }
}

// ── Step 1: planner tool exclusion (runner-level) ────────────────────────
