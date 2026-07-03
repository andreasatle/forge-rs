//! `debug_field`/`event_variant_name` extraction from pretty-printed Debug
//! dumps, and the node/attempt context-inheritance pass.

use super::super::parsing::{DefaultTraceParser, debug_field, event_variant_name};

#[test]
fn debug_field_extracts_quoted_string_and_unescapes_it() {
    let body = "RunNode {\n    node_id: NodeId(\n        \"root\",\n    ),\n    objective: \"line one\\nline two\",\n    kind: Work,\n}\n";
    assert_eq!(
        debug_field(body, "objective").as_deref(),
        Some("line one\nline two"),
        "escaped \\n inside a Debug-quoted string must become a real newline"
    );
}

#[test]
fn debug_field_extracts_bare_enum_unit_variant() {
    let body = "NodeFailed {\n    kind: ProtocolFailure,\n    message: \"boom\",\n}\n";
    assert_eq!(
        debug_field(body, "kind").as_deref(),
        Some("ProtocolFailure"),
        "a bare enum variant value has no quotes to strip, only the trailing comma"
    );
}

#[test]
fn debug_field_extracts_struct_variant_opener_without_its_body() {
    let body = "NodeFailed {\n    kind: ProtocolFailure,\n    message: \"boom\",\n    recovery: Terminal {\n        message: \"boom\",\n    },\n}\n";
    assert_eq!(
        debug_field(body, "recovery").as_deref(),
        Some("Terminal"),
        "a nested struct-variant field value is just its variant name, \
         with the dangling open-brace stripped"
    );
}

#[test]
fn debug_field_returns_first_match_when_field_name_repeats_at_different_nesting() {
    // NodeFailure's own `message` field is declared before the nested
    // `recovery: Terminal { message }`, so the outer (more useful) one wins.
    let body = "NodeFailed {\n    kind: ProtocolFailure,\n    message: \"outer reason\",\n    recovery: Terminal {\n        message: \"inner reason\",\n    },\n}\n";
    assert_eq!(
        debug_field(body, "message").as_deref(),
        Some("outer reason")
    );
}

#[test]
fn debug_field_returns_none_when_field_absent() {
    let body = "ToolLoopLimitReached\n";
    assert_eq!(debug_field(body, "objective"), None);
}

#[test]
fn event_variant_name_skips_machine_and_bare_marker_lines() {
    let body =
        "machine: DeliberationMachine\nevent:\nCriticAccepted {\n    content: \"looks good\",\n}\n";
    assert_eq!(event_variant_name(body), Some("CriticAccepted"));
}

#[test]
fn event_variant_name_handles_unit_variant_with_no_body() {
    let body = "machine: SchedulerMachine\nevent:\nStart\n";
    assert_eq!(event_variant_name(body), Some("Start"));
}

#[test]
fn parse_record_splits_header_and_keeps_full_body() {
    let content =
        "source: RoleMachine\nsubsource: Producer\nkind: ParseSucceeded\nattempt_count: 1\n";
    let record = DefaultTraceParser::parse_record(content).expect("well-formed content must parse");
    assert_eq!(record.source, "RoleMachine");
    assert_eq!(record.subsource.as_deref(), Some("Producer"));
    assert_eq!(record.kind, "ParseSucceeded");
    assert_eq!(record.body, "attempt_count: 1");
}

fn record_with_context(source: &str, kind: &str, node_id: &str, attempt: u32) -> String {
    format!("source: {source}\nnode_id: {node_id}\nattempt: {attempt}\nkind: {kind}\n")
}

fn record_without_context(source: &str, subsource: &str, kind: &str) -> String {
    format!("source: {source}\nsubsource: {subsource}\nkind: {kind}\n")
}

#[test]
fn context_less_record_inherits_the_last_explicit_node_and_attempt() {
    let records = vec![
        DefaultTraceParser::parse_record(&record_with_context(
            "SchedulerMachine",
            "EffectEmitted",
            "root",
            0,
        ))
        .unwrap(),
        DefaultTraceParser::parse_record(&record_without_context(
            "RoleMachine",
            "Producer",
            "ParseSucceeded",
        ))
        .unwrap(),
    ];

    let contextualized = DefaultTraceParser::new(&[]).assign_node_context(records);

    assert_eq!(contextualized.len(), 2);
    assert_eq!(contextualized[1].node_id, "root");
    assert_eq!(
        contextualized[1].attempt, 0,
        "a RoleMachine record with no node_id/attempt of its own must inherit \
         the most recently seen explicit context"
    );
}

#[test]
fn explicit_context_overrides_the_inherited_one() {
    let records = vec![
        DefaultTraceParser::parse_record(&record_with_context(
            "SchedulerMachine",
            "EffectEmitted",
            "root",
            0,
        ))
        .unwrap(),
        DefaultTraceParser::parse_record(&record_without_context(
            "RoleMachine",
            "Producer",
            "ParseSucceeded",
        ))
        .unwrap(),
        DefaultTraceParser::parse_record(&record_with_context(
            "SchedulerMachine",
            "EffectEmitted",
            "root-child-0",
            0,
        ))
        .unwrap(),
        DefaultTraceParser::parse_record(&record_without_context(
            "RoleMachine",
            "Producer",
            "ParseSucceeded",
        ))
        .unwrap(),
    ];

    let contextualized = DefaultTraceParser::new(&[]).assign_node_context(records);

    assert_eq!(contextualized[1].node_id, "root");
    assert_eq!(
        contextualized[3].node_id, "root-child-0",
        "a later explicit node_id must override the inherited context for \
         records that follow it"
    );
}

#[test]
fn records_before_any_node_context_are_dropped() {
    let records = vec![
        DefaultTraceParser::parse_record("source: SchedulerMachine\nkind: MachineStarted\n")
            .unwrap(),
    ];

    let contextualized = DefaultTraceParser::new(&[]).assign_node_context(records);

    assert!(
        contextualized.is_empty(),
        "a record with no node context established yet has nothing to attach to"
    );
}
