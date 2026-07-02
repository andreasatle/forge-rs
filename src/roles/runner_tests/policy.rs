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
        prompt.contains("`summary`"),
        "default policy must include JSON summary field; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("`status`"),
        "Work-node Producer prompt must never contain the status field; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"...\""),
        "default policy must not contain dot-placeholder JSON values; got:\n{prompt}"
    );
    assert!(
        !prompt.contains("\"rejected\""),
        "Work-node Producer prompt must never offer the rejected schema; got:\n{prompt}"
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
    // Critic and Referee use the status/content wrapper schema with both branches.
    for (label, system) in [
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
            prompt.contains("Accepted: `status` must be \"accepted\""),
            "{label} default policy must describe accepted schema; got:\n{prompt}"
        );
        assert!(
            prompt.contains("Rejected: `status` must be \"rejected\""),
            "{label} default policy must describe rejected schema; got:\n{prompt}"
        );
    }
    // Work-node Producer uses the accepted-only schema — it never rejects.
    let worker_prompt = render_role_prompt(
        &policy.worker_producer_system,
        &DeliberationRole::Producer,
        "test",
        None,
        None,
        &[],
        &[],
    );
    assert!(
        worker_prompt.contains("Return exactly one JSON object"),
        "worker default policy must include JSON-only instruction; got:\n{worker_prompt}"
    );
    assert!(
        worker_prompt.contains("`summary` must be a non-empty task-specific string"),
        "worker default policy must describe summary schema; got:\n{worker_prompt}"
    );
    assert!(
        !worker_prompt.contains("`status`"),
        "worker default policy must never include the status/content schema; got:\n{worker_prompt}"
    );
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
        planner_prompt.contains("`tasks`"),
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
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
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
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"work done"}"#]);
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

// ── language guidance ─────────────────────────────────────────────────────

#[test]
fn language_guidance_renders_between_system_prompt_and_tool_section() {
    // Invariant: when RolePolicy::language_guidance is set, it appears as its
    // own labeled section positioned after the adapter system prompt and
    // before the tool section, so it is identifiable in traces.
    let policy = RolePolicy {
        worker_producer_system: "SYSTEM_MARKER".to_string(),
        language_guidance: Some("LANGUAGE_MARKER".to_string()),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        with_dummy_tool_context(producer_request("do the work")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("Language guidance:\nLANGUAGE_MARKER"),
        "prompt must contain a labeled language guidance section; got:\n{prompt}"
    );
    let system_pos = prompt
        .find("SYSTEM_MARKER")
        .expect("system prompt must appear in prompt");
    let guidance_pos = prompt
        .find("Language guidance:")
        .expect("language guidance section must appear in prompt");
    let tool_pos = prompt
        .find("Available file tools")
        .expect("tool section must appear in prompt");
    assert!(
        system_pos < guidance_pos && guidance_pos < tool_pos,
        "language guidance must sit between the system prompt and the tool section; got:\n{prompt}"
    );
}

#[test]
fn no_language_guidance_section_when_unset() {
    // Invariant: RolePolicy::default() carries no language guidance, so the
    // prompt must not gain a stray "Language guidance:" section.
    let policy = RolePolicy::default();
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        with_dummy_tool_context(producer_request("do the work")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("Language guidance:"),
        "prompt must not contain a language guidance section when unset; got:\n{prompt}"
    );
}

#[test]
fn language_constraints_renders_after_language_guidance_section() {
    // Invariant: when RolePolicy::language_constraints is set, it appears as
    // its own labeled section positioned after "Language guidance:" and
    // before the tool section, so it is identifiable in traces and distinct
    // from general guidance.
    let policy = RolePolicy {
        worker_producer_system: "SYSTEM_MARKER".to_string(),
        language_guidance: Some("LANGUAGE_MARKER".to_string()),
        language_constraints: Some("CONSTRAINTS_MARKER".to_string()),
        ..RolePolicy::default()
    };
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        with_dummy_tool_context(producer_request("do the work")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        prompt.contains("Language constraints:\nCONSTRAINTS_MARKER"),
        "prompt must contain a labeled language constraints section; got:\n{prompt}"
    );
    let guidance_pos = prompt
        .find("Language guidance:")
        .expect("language guidance section must appear in prompt");
    let constraints_pos = prompt
        .find("Language constraints:")
        .expect("language constraints section must appear in prompt");
    let tool_pos = prompt
        .find("Available file tools")
        .expect("tool section must appear in prompt");
    assert!(
        guidance_pos < constraints_pos && constraints_pos < tool_pos,
        "language constraints must sit after language guidance and before the tool section; got:\n{prompt}"
    );
}

#[test]
fn no_language_constraints_section_when_unset() {
    // Invariant: RolePolicy::default() carries no language constraints, so
    // the prompt must not gain a stray "Language constraints:" section.
    let policy = RolePolicy::default();
    let provider = ScriptedProvider::from_strs(&[r#"{"summary":"completed"}"#]);
    let runner = ProviderRoleRunner::new_with_policy(&provider, policy);

    runner.run_role(
        with_dummy_tool_context(producer_request("do the work")),
        &crate::telemetry::NoopTelemetry,
    );

    let requests = provider.requests.borrow();
    let prompt = &requests[0].prompt;
    assert!(
        !prompt.contains("Language constraints:"),
        "prompt must not contain a language constraints section when unset; got:\n{prompt}"
    );
}

// ── Step 1: planner tool exclusion (runner-level) ────────────────────────
