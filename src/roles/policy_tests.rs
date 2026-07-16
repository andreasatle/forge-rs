use super::*;

#[test]
fn default_system_prompts_have_expected_role_schemas() {
    let policy = RolePolicy::default();

    let planner_producer_system = format!(
        "{}\n{}",
        policy.planner_producer_base,
        planner_protocol_schema_for(false)
    );
    assert_schema(
        &planner_producer_system,
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

#[test]
fn work_producer_system_does_not_assert_role_specific_action() {
    // Invariant: WORK_PRODUCER_SYSTEM is shared byte-for-byte across every
    // worker role (implementer, tester, pass_tests, ...), so it must not
    // assert what completing the task means — asserting "implement" would
    // contradict non-implementer roles, whose own Identity/Instructions
    // define the work in their own terms (tester writes tests, pass_tests
    // debugs an existing implementation against existing tests).
    assert!(
        !WORK_PRODUCER_SYSTEM.to_lowercase().contains("implement"),
        "shared Work-node Producer response contract must not hardcode \
         implementer-specific wording: {WORK_PRODUCER_SYSTEM}"
    );
}

#[test]
fn default_system_does_not_assert_role_specific_action() {
    // Invariant: DEFAULT_SYSTEM backs every Critic/Referee response contract
    // across every worker role, so — same as WORK_PRODUCER_SYSTEM above — it
    // must stay role-neutral rather than asserting implementer-specific
    // wording that would contradict a non-implementer role's own Identity.
    assert!(
        !DEFAULT_SYSTEM.to_lowercase().contains("implement"),
        "shared Critic/Referee response contract must not hardcode \
         implementer-specific wording: {DEFAULT_SYSTEM}"
    );
}

#[test]
fn worker_producer_identity_does_not_assert_role_specific_action() {
    // Invariant: WORKER_PRODUCER_IDENTITY is shared byte-for-byte across
    // every worker role, rendered immediately above each role's own Identity
    // sentence — same as WORK_PRODUCER_SYSTEM above, it must not assert what
    // completing the task means, since that would contradict non-implementer
    // roles, whose own Identity/Instructions define the work in their own
    // terms.
    assert!(
        !WORKER_PRODUCER_IDENTITY
            .to_lowercase()
            .contains("implement"),
        "shared Work-node Producer identity must not hardcode \
         implementer-specific wording: {WORKER_PRODUCER_IDENTITY}"
    );
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

    let plan = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose it","name":"fibonacci","function_name":"fibonacci","role_targets":[{"role":"implementer","file_path":"src/fibonacci.rs"}],"operation":"modify","targets":[],"depends_on":[]}]}"#;
    assert!(
        grammar.accepts(plan),
        "no-work grammar must accept kind: \"plan\""
    );

    let plan_missing_name = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose it","function_name":"fibonacci","role_targets":[{"role":"implementer","file_path":"src/fibonacci.rs"}],"operation":"modify","targets":[],"depends_on":[]}]}"#;
    assert!(
        !grammar.accepts(plan_missing_name),
        "no-work grammar must reject a kind: \"plan\" task missing the now-required name field"
    );

    let task = r#"{"kind":"task","tasks":[{"id":"t1","objective":"do it","name":"fibonacci","function_name":"fibonacci","role_targets":[{"role":"implementer","file_path":"src/fibonacci.rs"}],"depends_on":[]}]}"#;
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

    // `kind: "work"` tasks never become a terminal task row, so `name` stays
    // grammar-illegal for them — only `plan`/`task` require it.
    let work_with_name = r#"{"kind":"work","tasks":[{"id":"t1","objective":"do it","name":"fibonacci","operation":"modify","role":"implementer","targets":["a.txt"],"depends_on":[]}]}"#;
    assert!(
        !grammar.accepts(work_with_name),
        "with-roles grammar must reject a name field on a kind: \"work\" task"
    );
}

#[test]
fn planner_gbnf_with_roles_requires_name_on_kind_plan() {
    // Invariant: a `kind: "plan"` batch can collapse into a terminal task row
    // via the single-task short-circuit
    // (`PlannerOutputProcessor::into_plan`), so the with-roles grammar must
    // require `name` on every `kind: "plan"` task, same as `kind: "task"`.
    use super::gbnf_check::Grammar;

    let grammar = Grammar::parse(PLANNER_GBNF_WITH_ROLES);

    let plan = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose it","name":"fibonacci","function_name":"fibonacci","role_targets":[{"role":"implementer","file_path":"src/fibonacci.rs"}],"operation":"modify","role":"implementer","targets":[],"depends_on":[]}]}"#;
    assert!(
        grammar.accepts(plan),
        "with-roles grammar must accept kind: \"plan\" with a name"
    );

    let plan_missing_name = r#"{"kind":"plan","tasks":[{"id":"t1","objective":"decompose it","function_name":"fibonacci","role_targets":[{"role":"implementer","file_path":"src/fibonacci.rs"}],"operation":"modify","role":"implementer","targets":[],"depends_on":[]}]}"#;
    assert!(
        !grammar.accepts(plan_missing_name),
        "with-roles grammar must reject a kind: \"plan\" task missing the now-required name field"
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
