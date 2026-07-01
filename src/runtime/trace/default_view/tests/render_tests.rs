//! Exact-format assertions for the default view's text rendering, given
//! hand-built fixtures (no telemetry parsing involved).

use super::super::render::render;
use super::super::{AttemptEvent, AttemptSummary, NodeStatus, NodeSummary};

fn accepted_node(node_id: &str, kind: &str, objective: &str) -> NodeSummary {
    NodeSummary {
        node_id: node_id.to_string(),
        kind: Some(kind.to_string()),
        objective: Some(objective.to_string()),
        attempts: vec![AttemptSummary {
            attempt: 0,
            events: vec![
                AttemptEvent::Producer {
                    completed: true,
                    tool_calls: 1,
                },
                AttemptEvent::Validator {
                    accepted: true,
                    reason: None,
                },
                AttemptEvent::Critic {
                    accepted: true,
                    rationale: "fine".to_string(),
                },
                AttemptEvent::Referee {
                    accepted: true,
                    rationale: "fine".to_string(),
                },
            ],
        }],
        status: NodeStatus::Accepted,
        last_failure: None,
    }
}

#[test]
fn header_lines_render_run_id_objective_and_event_count() {
    let out = render("2026-06-22-15-31-42", Some("build the thing"), 42, &[]);
    assert!(
        out.starts_with(
            "run_id: 2026-06-22-15-31-42\nobjective: build the thing\nevent_count: 42\n"
        )
    );
}

#[test]
fn missing_objective_renders_as_unknown() {
    let out = render("run-1", None, 0, &[]);
    assert!(out.contains("objective: (unknown)\n"));
}

#[test]
fn node_list_line_has_no_last_bracket_when_accepted() {
    let node = accepted_node("root", "Work", "write a function");
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));
    assert!(
        out.contains("  root  kind=Work  status=accepted  attempts=1\n"),
        "accepted nodes must not show a trailing [last: ...] bracket; got:\n{out}"
    );
}

#[test]
fn node_list_line_shows_last_bracket_when_failed() {
    let mut node = accepted_node("root", "Work", "write a function");
    node.status = NodeStatus::Failed;
    node.last_failure = Some("provider timed out".to_string());

    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));
    assert!(
        out.contains("  root  kind=Work  status=failed  attempts=1  [last: provider timed out]\n"),
        "failed nodes must show the terminal failure summary; got:\n{out}"
    );
}

#[test]
fn unknown_kind_and_objective_render_as_placeholders() {
    let node = NodeSummary {
        node_id: "root".to_string(),
        kind: None,
        objective: None,
        attempts: vec![],
        status: NodeStatus::Unknown,
        last_failure: None,
    };
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));
    assert!(out.contains("kind=unknown"));
    assert!(
        out.lines().any(|l| l == "    (unknown)"),
        "missing node objective must render as a placeholder; got:\n{out}"
    );
}

#[test]
fn timeline_indents_objective_and_attempt_blocks() {
    let node = accepted_node("root", "Work", "write a function");
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));

    let expected_timeline = "\
timeline:
node root:
  objective:
    write a function

  attempt 0:
    producer: completed [tool_calls=1]
    validator: accepted
    critic: accept fine
    referee: accept fine";
    assert!(
        out.contains(expected_timeline),
        "timeline block did not match expected indentation; got:\n{out}"
    );
}

#[test]
fn validator_reason_is_bracketed_only_when_rejected() {
    let mut node = accepted_node("root", "Work", "obj");
    node.attempts[0].events[1] = AttemptEvent::Validator {
        accepted: false,
        reason: Some("missing return type".to_string()),
    };
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));
    assert!(out.contains("    validator: rejected [reason: missing return type]\n"));
}

#[test]
fn critic_and_referee_rationale_truncated_at_120_chars() {
    let mut node = accepted_node("root", "Work", "obj");
    let long = "x".repeat(200);
    node.attempts[0].events[2] = AttemptEvent::Critic {
        accepted: true,
        rationale: long.clone(),
    };
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));

    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with("critic:"))
        .unwrap();
    let rationale = line.trim_start().strip_prefix("critic: accept ").unwrap();
    assert_eq!(
        rationale.chars().count(),
        121,
        "120 chars plus the ellipsis appended by truncate()"
    );
    assert!(rationale.ends_with('…'));
}

#[test]
fn role_failed_line_has_no_brackets() {
    let mut node = accepted_node("root", "Work", "obj");
    node.attempts[0].events = vec![AttemptEvent::RoleFailed {
        kind: "ProtocolFailure".to_string(),
        phase: "Producer".to_string(),
        summary: "provider timed out".to_string(),
    }];
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));
    assert!(
        out.lines()
            .any(|l| l == "    failed: ProtocolFailure Producer provider timed out"),
        "got:\n{out}"
    );
}

#[test]
fn validation_failed_block_shows_command_exit_and_indented_lines() {
    let mut node = accepted_node("root", "Work", "obj");
    node.attempts[0].events = vec![AttemptEvent::ValidationFailed {
        command: Some("cargo test".to_string()),
        exit_code: Some(1),
        lines: vec!["thread panicked".to_string(), "left != right".to_string()],
    }];
    let out = render("run-1", Some("obj"), 1, std::slice::from_ref(&node));

    let expected = "\
    validation failed: cargo test exit=1
      thread panicked
      left != right";
    assert!(out.contains(expected), "got:\n{out}");
}

#[test]
fn multiple_nodes_are_separated_by_a_blank_line_in_the_timeline() {
    let a = accepted_node("root", "Plan", "decompose");
    let b = accepted_node("root-child-0", "Work", "first task");
    let out = render("run-1", Some("obj"), 2, &[a, b]);

    let timeline = out.split("timeline:\n").nth(1).unwrap();
    let node_positions: Vec<_> = timeline.match_indices("node ").map(|(i, _)| i).collect();
    assert_eq!(node_positions.len(), 2);
    let between = &timeline[..node_positions[1]];
    assert!(
        between.trim_end().ends_with("referee: accept fine"),
        "expected the first node's block to end right before the blank separator; got:\n{between}"
    );
    assert!(
        between.ends_with("fine\n\n"),
        "a blank line must separate consecutive node blocks in the timeline; got:\n{between:?}"
    );
}
