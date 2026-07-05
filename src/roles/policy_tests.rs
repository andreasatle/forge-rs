use super::*;

#[test]
fn default_system_prompts_have_expected_role_schemas() {
    let policy = RolePolicy::default();

    assert_schema(
        &policy.planner_producer_system,
        &["`tasks`"],
        &["`status`", "`summary`"],
    );
    assert_schema(
        &policy.worker_producer_system,
        &["`summary`"],
        &["`status`", "`tasks`"],
    );

    for system in [
        &policy.planner_critic_system,
        &policy.worker_critic_system,
        &policy.planner_referee_system,
        &policy.worker_referee_system,
    ] {
        assert_schema(
            system,
            &["`status`", "`content`", "`reason`"],
            &["`summary`", "`tasks`"],
        );
    }
}

#[test]
fn default_system_prompts_have_no_placeholder_values() {
    let policy = RolePolicy::default();
    for system in [
        &policy.planner_producer_system,
        &policy.worker_producer_system,
        &policy.planner_critic_system,
        &policy.worker_critic_system,
        &policy.planner_referee_system,
        &policy.worker_referee_system,
        &policy.planner_protocol_schema,
        &policy.planner_producer_base,
    ] {
        assert!(
            !system.contains('$'),
            "system prompt contains `$`: {system}"
        );
        assert!(
            !system.contains("\"...\""),
            "system prompt contains placeholder JSON value: {system}"
        );
    }
}

fn assert_schema(system: &str, required: &[&str], forbidden: &[&str]) {
    for field in required {
        assert!(
            system.contains(field),
            "schema is missing {field}: {system}"
        );
    }
    for field in forbidden {
        assert!(
            !system.contains(field),
            "schema includes unexpected {field}: {system}"
        );
    }
}

#[test]
fn planner_producer_base_plus_configured_footer_reconstructs_producer_system() {
    // Invariant: `planner_producer_system` is exactly `planner_producer_base`
    // followed by the adapter-configured `planner_protocol_schema` footer —
    // callers rely on this to build fixed-schema variants for
    // Decomposition/Plan nodes from the same base text.
    let policy = RolePolicy::default();
    assert_eq!(
        policy.planner_producer_system,
        format!(
            "{}\n{}",
            policy.planner_producer_base, policy.planner_protocol_schema
        )
    );
}

#[test]
fn planner_protocol_schema_for_selects_fixed_schema_by_node_kind() {
    // Invariant: Decomposition always gets the no-operation, no-role
    // schema and Plan always gets the with-operation, with-roles schema,
    // regardless of what the adapter configured; Work defers to the
    // adapter-configured `planner_protocol_schema`.
    let policy = RolePolicy {
        planner_protocol_schema: "custom adapter schema".to_string(),
        ..RolePolicy::default()
    };

    assert_eq!(
        planner_protocol_schema_for(&NodeKind::Decomposition, &policy),
        PLANNER_PROTOCOL_FOOTER
    );
    assert_eq!(
        planner_protocol_schema_for(&NodeKind::Plan, &policy),
        PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES
    );
    assert_eq!(
        planner_protocol_schema_for(&NodeKind::Work, &policy),
        "custom adapter schema"
    );
}
