//! `EventHeader::parse` invariants: the trace views all depend on this
//! correctly separating the record's routing header (source/subsource/
//! node_id/attempt/kind) from its first-field preview.

use std::path::Path;

use crate::runtime::trace::reader::{EventHeader, strip_trailing_open_bracket, truncate};
use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::temp_dir;

#[test]
fn event_header_parses_source_and_kind_without_subsource() {
    let dir = temp_dir("header-no-sub");
    let sink = FileTelemetry::new(dir.clone());
    sink.record(TelemetryRecord::new(
        "SchedulerMachine",
        TelemetryEvent::MachineStarted {
            machine: "SchedulerHandler".into(),
        },
    ));
    let path = dir.join("000001--scheduler-machine--machine-started.txt");
    let content = std::fs::read_to_string(&path).unwrap();

    let header = EventHeader::parse(&path, &content).expect("header must parse");
    assert_eq!(header.counter, "000001");
    assert_eq!(header.source, "SchedulerMachine");
    assert_eq!(header.subsource, None);
    assert_eq!(header.kind, "MachineStarted");
    // MachineStarted's only field is `machine:`, which always repeats
    // `source`; skipping it leaves no further field to preview.
    assert_eq!(header.preview, None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn event_header_parses_subsource_when_present() {
    let dir = temp_dir("header-with-sub");
    let sink = FileTelemetry::new(dir.clone());
    sink.record(TelemetryRecord::new_with_subsource(
        "RoleMachine",
        "Producer",
        TelemetryEvent::ParseSucceeded { attempt_count: 1 },
    ));
    let path = dir.join("000001--role-machine--producer--parse-succeeded.txt");
    let content = std::fs::read_to_string(&path).unwrap();

    let header = EventHeader::parse(&path, &content).expect("header must parse");
    assert_eq!(header.source, "RoleMachine");
    assert_eq!(header.subsource, Some("Producer".to_string()));
    assert_eq!(header.kind, "ParseSucceeded");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn event_header_parses_node_id_and_attempt_when_present() {
    let dir = temp_dir("header-with-node-context");
    let sink = FileTelemetry::new(dir.clone());
    let mut record = TelemetryRecord::new(
        "DeliberationMachine",
        TelemetryEvent::StateEntered {
            machine: "DeliberationMachine".into(),
            state: "Ready".into(),
        },
    );
    record.node_id = Some("root-child-0".into());
    record.attempt = Some(1);
    sink.record(record);
    let path = dir.join("000001--deliberation-machine--state-entered.txt");
    let content = std::fs::read_to_string(&path).unwrap();

    let header = EventHeader::parse(&path, &content).expect("header must parse");
    assert_eq!(header.node_id.as_deref(), Some("root-child-0"));
    assert_eq!(header.attempt.as_deref(), Some("1"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn event_header_leaves_node_id_and_attempt_none_when_absent() {
    let dir = temp_dir("header-without-node-context");
    let sink = FileTelemetry::new(dir.clone());
    sink.record(TelemetryRecord::new(
        "SchedulerMachine",
        TelemetryEvent::MachineStarted {
            machine: "SchedulerHandler".into(),
        },
    ));
    let path = dir.join("000001--scheduler-machine--machine-started.txt");
    let content = std::fs::read_to_string(&path).unwrap();

    let header = EventHeader::parse(&path, &content).expect("header must parse");
    assert_eq!(header.node_id, None);
    assert_eq!(header.attempt, None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn event_header_preview_skips_redundant_machine_field_and_bare_marker() {
    let dir = temp_dir("header-multiline-marker");
    let sink = FileTelemetry::new(dir.clone());
    sink.record(TelemetryRecord::new(
        "SchedulerMachine",
        TelemetryEvent::StateEntered {
            machine: "SchedulerMachine".into(),
            state: "Active { graph: .. }".into(),
        },
    ));
    let path = dir.join("000001--scheduler-machine--state-entered.txt");
    let content = std::fs::read_to_string(&path).unwrap();

    let header = EventHeader::parse(&path, &content).expect("header must parse");
    assert_eq!(
        header.preview.as_deref(),
        Some("Active { graph: .. }"),
        "preview must skip the redundant `machine:` field (it always repeats `source`) \
         and the bare `state:` marker, landing on the actual state text"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn event_header_preview_uses_first_non_redundant_single_line_field() {
    // Failure has no `machine:` field, so its first field (`component:`)
    // is used directly.
    let content = "source: SchedulerMachine\nkind: Failure\ncomponent: Worker\nreason: boom\n";
    let path = Path::new("000001--scheduler-machine--failure.txt");

    let header = EventHeader::parse(path, content).expect("header must parse");
    assert_eq!(header.preview.as_deref(), Some("component: Worker"));
}

#[test]
fn event_header_preview_skips_bare_multiline_marker() {
    // Synthetic content where a bare `state:` marker (no inline value,
    // and no preceding `machine:` field) is the first line after `kind:`.
    let content = "source: SchedulerMachine\nkind: StateEntered\nstate:\nActive { graph: .. }\n";
    let path = Path::new("000001--scheduler-machine--state-entered.txt");

    let header = EventHeader::parse(path, content).expect("header must parse");
    assert_eq!(
        header.preview.as_deref(),
        Some("Active { graph: .. }"),
        "a bare multi-line marker must be skipped in favor of its first content line"
    );
}

#[test]
fn event_header_preview_is_none_when_no_further_fields() {
    let dir = temp_dir("header-no-preview");
    let sink = FileTelemetry::new(dir.clone());
    sink.record(TelemetryRecord::new(
        "RoleMachine",
        TelemetryEvent::ToolLoopLimitReached,
    ));
    let path = dir.join("000001--role-machine--tool-loop-limit-reached.txt");
    let content = std::fs::read_to_string(&path).unwrap();

    let header = EventHeader::parse(&path, &content).expect("header must parse");
    assert_eq!(
        header.preview, None,
        "an event with no fields after `kind:` must have no preview"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preview_strips_dangling_brace_from_pretty_printed_struct_variant() {
    // A real `{:#?}` dump of a struct-like enum variant puts only the
    // variant name and opening brace on the first line.
    let content = "source: SchedulerMachine\nkind: StateEntered\nstate:\n\
        Active {\n    graph: RunGraph {\n        nodes: [],\n    },\n}\n";
    let path = Path::new("000001--scheduler-machine--state-entered.txt");

    let header = EventHeader::parse(path, content).expect("header must parse");
    assert_eq!(
        header.preview.as_deref(),
        Some("Active"),
        "the dangling `{{` left by pretty-printed Debug output must be stripped"
    );
}

#[test]
fn preview_strips_dangling_paren_from_pretty_printed_tuple_variant() {
    let content = "source: SchedulerMachine\nkind: EventReceived\nevent:\n\
        WorkAccepted(\n    NodeId(\n        \"root\",\n    ),\n)\n";
    let path = Path::new("000001--scheduler-machine--event-received.txt");

    let header = EventHeader::parse(path, content).expect("header must parse");
    assert_eq!(header.preview.as_deref(), Some("WorkAccepted"));
}

#[test]
fn strip_trailing_open_bracket_handles_preview_line_variants() {
    let cases = [
        ("brace and trailing space", "Active {", "Active"),
        ("paren", "WorkAccepted(", "WorkAccepted"),
        (
            "complete line unchanged",
            "component: Worker",
            "component: Worker",
        ),
        ("bare bracket", "{", ""),
    ];

    for (name, input, expected) in cases {
        assert_eq!(strip_trailing_open_bracket(input), expected, "{name}");
    }
}

#[test]
fn truncate_handles_short_and_long_strings() {
    let long = "a".repeat(100);
    let cases = [
        ("short unchanged", "short", 80, "short".to_string()),
        (
            "long shortened with ellipsis",
            long.as_str(),
            10,
            format!("{}…", "a".repeat(10)),
        ),
    ];

    for (name, input, max_chars, expected) in cases {
        assert_eq!(truncate(input, max_chars), expected, "{name}");
    }
}
