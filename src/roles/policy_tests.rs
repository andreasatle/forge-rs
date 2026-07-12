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
fn planner_producer_base_plus_protocol_footer_reconstructs_producer_system() {
    // Invariant: `planner_producer_system` is exactly `planner_producer_base`
    // followed by the default `PLANNER_PROTOCOL_FOOTER` footer — callers rely
    // on this to build fixed-schema variants for Decomposition/Plan nodes
    // from the same base text.
    let policy = RolePolicy::default();
    assert_eq!(
        policy.planner_producer_system,
        format!(
            "{}\n{PLANNER_PROTOCOL_FOOTER}",
            policy.planner_producer_base
        )
    );
}

#[test]
fn planner_gbnf_no_work_rejects_kind_work_but_accepts_plan_and_task() {
    // Invariant: the grammar sent to the provider for a workerless Plan
    // Producer must genuinely reject `kind: "work"` at the grammar level —
    // not merely omit it from the Rust-side footer text. A real
    // grammar-constrained provider can never emit `kind: "work"` under this
    // grammar, closing the gap that let node `ba8a9160` (adapter `""`,
    // `worker_role: "Producer"` — a role that did not exist anywhere) slip
    // through as an orphaned, unvalidated Work node.
    use super::gbnf_check::Grammar;

    let grammar = Grammar::parse(PLANNER_GBNF_NO_WORK);

    let work = r#"{"kind":"work","tasks":[{"id":"t1","objective":"do it","operation":"modify","role":"implementer","targets":["a.txt"],"depends_on":[]}]}"#;
    assert!(
        !grammar.accepts(work),
        "no-work grammar must reject an explicit kind: \"work\""
    );

    // No default to fall back on either: omitting `kind` entirely must also
    // be rejected, since this grammar has no optional kind-field production.
    let omitted = r#"{"tasks":[{"id":"t1","objective":"do it","operation":"modify","targets":["a.txt"],"depends_on":[]}]}"#;
    assert!(
        !grammar.accepts(omitted),
        "no-work grammar must reject an omitted kind field"
    );

    let plan = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose it","operation":"modify","targets":[],"depends_on":[]}]}"#;
    assert!(
        grammar.accepts(plan),
        "no-work grammar must accept kind: \"plan\""
    );

    let task = r#"{"kind":"task","tasks":[{"id":"t1","objective":"do it","name":"fibonacci","depends_on":[]}]}"#;
    assert!(
        grammar.accepts(task),
        "no-work grammar must accept kind: \"task\""
    );
}

#[test]
fn planner_gbnf_with_roles_still_accepts_kind_work() {
    // Regression guard: the with-roles grammar variant (used when the
    // adapter defines worker roles) is unchanged by this fix — kind: "work"
    // and the default-omitted case both remain grammar-legal.
    use super::gbnf_check::Grammar;

    let grammar = Grammar::parse(PLANNER_GBNF_WITH_ROLES);

    let work = r#"{"kind":"work","tasks":[{"id":"t1","objective":"do it","operation":"modify","role":"implementer","targets":["a.txt"],"depends_on":[]}]}"#;
    assert!(
        grammar.accepts(work),
        "with-roles grammar must still accept kind: \"work\""
    );

    let omitted = r#"{"tasks":[{"id":"t1","objective":"do it","operation":"modify","role":"implementer","targets":["a.txt"],"depends_on":[]}]}"#;
    assert!(
        grammar.accepts(omitted),
        "with-roles grammar must still accept an omitted kind (defaults to work)"
    );
}

#[test]
fn planner_protocol_schema_for_selects_by_worker_role_presence() {
    // Invariant: the Plan Producer's schema depends on whether the adapter
    // defines any worker roles — `kind: "work"` is only offered when there
    // is at least one role to assign a work task to.
    assert_eq!(
        planner_protocol_schema_for(true),
        PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES
    );
    assert_eq!(
        planner_protocol_schema_for(false),
        PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_NO_WORK
    );
}
