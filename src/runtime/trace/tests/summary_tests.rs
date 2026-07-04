//! Flat summary-line formatting and full failure/prompt content rendering.

use crate::runtime::trace::reader::EventHeader;
use crate::runtime::trace::summary::{
    failure_body, is_failure_kind, json_to_yaml, prompt_body, summary_line,
};

#[test]
fn summary_line_formats_optional_context_and_preview() {
    let cases = [
        (
            // node_id is shortened to its first 8 characters in the
            // rendered line, matching the [worker <short-id>] log format.
            "node and attempt",
            EventHeader {
                counter: "000012".to_string(),
                source: "DeliberationMachine".to_string(),
                subsource: None,
                node_id: Some("root-child-0".to_string()),
                attempt: Some("1".to_string()),
                kind: "StateEntered".to_string(),
                preview: None,
            },
            "000012  DeliberationMachine  StateEntered  node=root-chi  attempt=1",
        ),
        (
            "preview",
            EventHeader {
                counter: "000001".to_string(),
                source: "SchedulerMachine".to_string(),
                subsource: None,
                node_id: None,
                attempt: None,
                kind: "MachineStarted".to_string(),
                preview: Some("machine: SchedulerHandler".to_string()),
            },
            "000001  SchedulerMachine  MachineStarted  machine: SchedulerHandler",
        ),
        (
            "no trailing separator without preview",
            EventHeader {
                counter: "000001".to_string(),
                source: "SchedulerMachine".to_string(),
                subsource: None,
                node_id: None,
                attempt: None,
                kind: "ValidationStarted".to_string(),
                preview: None,
            },
            "000001  SchedulerMachine  ValidationStarted",
        ),
    ];

    for (name, header, expected) in cases {
        assert_eq!(summary_line(&header), expected, "{name}");
    }
}

#[test]
fn event_header_display_formats_optional_subsource() {
    let cases = [
        (
            "with subsource",
            EventHeader {
                counter: "000001".to_string(),
                source: "RoleMachine".to_string(),
                subsource: Some("Producer".to_string()),
                node_id: None,
                attempt: None,
                kind: "RolePromptRendered".to_string(),
                preview: None,
            },
            "000001  RoleMachine/Producer  RolePromptRendered",
        ),
        (
            "without subsource",
            EventHeader {
                counter: "000002".to_string(),
                source: "SchedulerMachine".to_string(),
                subsource: None,
                node_id: None,
                attempt: None,
                kind: "MachineStarted".to_string(),
                preview: None,
            },
            "000002  SchedulerMachine  MachineStarted",
        ),
    ];

    for (name, header, expected) in cases {
        assert_eq!(header.to_string(), expected, "{name}");
    }
}

#[test]
fn failure_kinds_are_recognized() {
    assert!(is_failure_kind("Failure"));
    assert!(is_failure_kind("FailureClassified"));
    assert!(is_failure_kind("ValidationFailed"));
    assert!(is_failure_kind("ParseFailed"));
    assert!(!is_failure_kind("RolePromptRendered"));
    assert!(!is_failure_kind("MachineStarted"));
}

#[test]
fn prompt_body_extracts_text_after_marker_verbatim() {
    let content = "source: RoleMachine\nsubsource: Producer\nkind: RolePromptRendered\n\
        attempt_count: 1\nprompt:\nWrite a function.\n{\"example\":\"json in prompt\"}\n";
    assert_eq!(
        prompt_body(content),
        "Write a function.\n{\"example\":\"json in prompt\"}",
        "prompt body must be returned byte-for-byte, including any embedded JSON snippets"
    );
}

#[test]
fn prompt_body_falls_back_to_full_content_without_marker() {
    let content = "source: RoleMachine\nkind: SomethingElse\nfield: value\n";
    assert_eq!(
        prompt_body(content),
        "source: RoleMachine\nkind: SomethingElse\nfield: value"
    );
}

#[test]
fn json_to_yaml_converts_valid_json() {
    // serde_json orders object keys alphabetically without the
    // "preserve_order" feature, so `content` sorts before `status`.
    let yaml =
        json_to_yaml(r#"{"status":"accepted","content":"done"}"#).expect("valid JSON must convert");
    assert_eq!(yaml, "content: done\nstatus: accepted");
}

#[test]
fn json_to_yaml_returns_none_for_non_json() {
    assert!(
        json_to_yaml("not json at all").is_none(),
        "plain text must not be reported as convertible"
    );
}

#[test]
fn failure_body_renders_raw_response_json_as_yaml() {
    let content = "source: RoleMachine\nsubsource: Producer\nkind: ParseFailed\n\
        attempt_count: 1\nparse_error: missing field `status`\n\
        raw_response:\n{\"content\":\"done\"}\n";
    let body = failure_body(content);
    assert!(
        body.contains("parse_error: missing field `status`"),
        "fields before raw_response must be preserved verbatim; got: {body}"
    );
    assert!(
        body.ends_with("content: done"),
        "raw_response JSON must be rendered as YAML; got: {body}"
    );
    assert!(
        !body.contains("{\"content\":\"done\"}"),
        "raw JSON form must not remain once converted; got: {body}"
    );
}

#[test]
fn failure_body_leaves_non_json_raw_response_untouched() {
    let content = "source: RoleMachine\nkind: ParseFailed\nattempt_count: 1\n\
        parse_error: empty response\nraw_response:\n(empty)\n";
    let body = failure_body(content);
    assert!(
        body.ends_with("(empty)"),
        "non-JSON raw_response must be printed verbatim; got: {body}"
    );
}

#[test]
fn failure_body_without_raw_response_marker_is_unchanged() {
    let content = "source: SchedulerMachine\nkind: Failure\ncomponent: Worker\nreason: boom\n";
    assert_eq!(
        failure_body(content),
        "source: SchedulerMachine\nkind: Failure\ncomponent: Worker\nreason: boom"
    );
}
