//! Node/attempt reconstruction from a context-assigned telemetry stream.
//!
//! Each test builds a small sequence of hand-written telemetry file bodies
//! (mirroring real `{:#?}` Debug dumps) and checks one invariant of
//! `group_into_nodes`.

use super::super::grouping::DefaultTraceGrouper;
use super::super::parsing::DefaultTraceParser;
use super::super::{AttemptEvent, NodeStatus, NodeSummary};

fn run_node(node_id: &str, attempt: u32, kind: &str, objective: &str) -> String {
    format!(
        "source: SchedulerMachine\nnode_id: {node_id}\nattempt: {attempt}\nkind: EffectEmitted\nmachine: SchedulerMachine\neffect:\nRunNode {{\n    kind: {kind},\n    objective: \"{objective}\",\n}}\n"
    )
}

fn producer_tool_requested() -> String {
    "source: RoleMachine\nsubsource: Producer\nkind: ToolRequested\ntool: write_file\n".to_string()
}

fn producer_parse_succeeded() -> String {
    "source: RoleMachine\nsubsource: Producer\nkind: ParseSucceeded\nattempt_count: 1\n".to_string()
}

fn deliberation_event(node_id: &str, attempt: u32, variant_body: &str) -> String {
    format!(
        "source: DeliberationMachine\nnode_id: {node_id}\nattempt: {attempt}\nkind: EventReceived\nmachine: DeliberationMachine\nevent:\n{variant_body}\n"
    )
}

fn producer_validation_accepted(node_id: &str, attempt: u32) -> String {
    deliberation_event(
        node_id,
        attempt,
        "ProducerValidationAccepted {\n    content: \"fn foo() {}\",\n}",
    )
}

fn producer_validation_rejected(node_id: &str, attempt: u32, feedback: &str) -> String {
    deliberation_event(
        node_id,
        attempt,
        &format!(
            "ProducerValidationRejected {{\n    content: \"fn foo() {{}}\",\n    retry: ProducerValidationRetry {{\n        feedback_reason: \"{feedback}\",\n        max_retries: 2,\n        failure_kind: WorkSemanticValidationFailure,\n        failure_reason: \"n/a\",\n    }},\n}}"
        ),
    )
}

fn critic_accepted(node_id: &str, attempt: u32, content: &str) -> String {
    deliberation_event(
        node_id,
        attempt,
        &format!("CriticAccepted {{\n    content: \"{content}\",\n}}"),
    )
}

fn critic_rejected(node_id: &str, attempt: u32, reason: &str) -> String {
    deliberation_event(
        node_id,
        attempt,
        &format!("CriticRejected {{\n    reason: \"{reason}\",\n}}"),
    )
}

fn referee_accepted(node_id: &str, attempt: u32, content: &str) -> String {
    deliberation_event(
        node_id,
        attempt,
        &format!("RefereeAccepted {{\n    content: \"{content}\",\n}}"),
    )
}

fn referee_rejected(node_id: &str, attempt: u32, reason: &str) -> String {
    deliberation_event(
        node_id,
        attempt,
        &format!("RefereeRejected {{\n    reason: \"{reason}\",\n}}"),
    )
}

fn producer_failed(node_id: &str, attempt: u32, kind: &str, reason: &str) -> String {
    deliberation_event(
        node_id,
        attempt,
        &format!("ProducerFailed {{\n    kind: {kind},\n    reason: \"{reason}\",\n}}"),
    )
}

fn scheduler_event(variant_body: &str) -> String {
    format!(
        "source: SchedulerMachine\nkind: EventReceived\nmachine: SchedulerMachine\nevent:\n{variant_body}\n"
    )
}

fn integration_succeeded(node_id: &str) -> String {
    scheduler_event(&format!(
        "IntegrationSucceeded {{\n    node_id: NodeId(\n        \"{node_id}\",\n    ),\n    output: IntegrationOutput {{\n        summary: \"committed\",\n    }},\n}}"
    ))
}

fn plan_accepted(node_id: &str) -> String {
    scheduler_event(&format!(
        "PlanAccepted {{\n    node_id: NodeId(\n        \"{node_id}\",\n    ),\n}}"
    ))
}

fn node_failed(node_id: &str, kind: &str, message: &str, recovery: &str) -> String {
    scheduler_event(&format!(
        "NodeFailed {{\n    node_id: NodeId(\n        \"{node_id}\",\n    ),\n    failure: NodeFailure {{\n        kind: {kind},\n        message: \"{message}\",\n        recovery: {recovery} {{\n            message: \"{message}\",\n        }},\n    }},\n}}"
    ))
}

fn validation_failed(command: &str, exit_code: i32, stdout: &[&str], stderr: &[&str]) -> String {
    format!(
        "source: Integration\nkind: ValidationFailed\nsummary: validation failed\ncommand: {command}\nexit_code: {exit_code}\nstdout:\n{}\nstderr:\n{}\n",
        stdout.join("\n"),
        stderr.join("\n"),
    )
}

fn build(contents: Vec<String>) -> Vec<NodeSummary> {
    let records: Vec<_> = contents
        .iter()
        .map(|c| DefaultTraceParser::parse_record(c).unwrap())
        .collect();
    let contextualized = DefaultTraceParser::new(&[]).assign_node_context(records);
    DefaultTraceGrouper::new().group(contextualized)
}

#[test]
fn clean_accept_path_produces_one_line_per_role() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        producer_tool_requested(),
        producer_tool_requested(),
        producer_parse_succeeded(),
        producer_validation_accepted("root", 0),
        critic_accepted("root", 0, "looks good"),
        referee_accepted("root", 0, "ship it"),
        integration_succeeded("root"),
    ]);

    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    assert_eq!(node.kind.as_deref(), Some("Work"));
    assert_eq!(node.objective.as_deref(), Some("write a function"));
    assert_eq!(node.status, NodeStatus::Accepted);
    assert_eq!(node.attempts.len(), 1);
    assert_eq!(
        node.attempts[0].events,
        vec![
            AttemptEvent::Producer {
                completed: true,
                tool_calls: 2
            },
            AttemptEvent::Validator {
                accepted: true,
                reason: None
            },
            AttemptEvent::Critic {
                accepted: true,
                rationale: "looks good".to_string()
            },
            AttemptEvent::Referee {
                accepted: true,
                rationale: "ship it".to_string()
            },
        ],
        "a single clean round must produce exactly one line per role, in order"
    );
}

#[test]
fn producer_validation_rejection_causes_a_second_producer_round_marked_retry() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        producer_parse_succeeded(),
        producer_validation_rejected("root", 0, "missing docstring"),
        producer_parse_succeeded(),
        producer_validation_accepted("root", 0),
        critic_accepted("root", 0, "ok"),
        referee_accepted("root", 0, "ok"),
    ]);

    assert_eq!(
        nodes[0].attempts[0].events,
        vec![
            AttemptEvent::Producer {
                completed: true,
                tool_calls: 0
            },
            AttemptEvent::Validator {
                accepted: false,
                reason: Some("missing docstring".to_string())
            },
            AttemptEvent::Producer {
                completed: false,
                tool_calls: 0
            },
            AttemptEvent::Validator {
                accepted: true,
                reason: None
            },
            AttemptEvent::Critic {
                accepted: true,
                rationale: "ok".to_string()
            },
            AttemptEvent::Referee {
                accepted: true,
                rationale: "ok".to_string()
            },
        ],
        "a rejected producer round followed by a second round within the same \
         attempt must mark only the second round as a retry"
    );
}

#[test]
fn referee_rejection_loops_back_to_a_new_producer_round_in_the_same_attempt() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        producer_parse_succeeded(),
        producer_validation_accepted("root", 0),
        critic_rejected("root", 0, "needs more tests"),
        referee_rejected("root", 0, "not ready, revise"),
        producer_parse_succeeded(),
        producer_validation_accepted("root", 0),
        critic_accepted("root", 0, "ok now"),
        referee_accepted("root", 0, "ok now"),
    ]);

    let events = &nodes[0].attempts[0].events;
    assert_eq!(
        nodes[0].attempts.len(),
        1,
        "revision loop stays within one attempt"
    );
    assert_eq!(
        events[2],
        AttemptEvent::Critic {
            accepted: false,
            rationale: "needs more tests".to_string()
        }
    );
    assert_eq!(
        events[3],
        AttemptEvent::Referee {
            accepted: false,
            rationale: "not ready, revise".to_string()
        }
    );
    assert_eq!(
        events[4],
        AttemptEvent::Producer {
            completed: false,
            tool_calls: 0
        },
        "the producer round after a referee rejection must be reported as a retry"
    );
}

#[test]
fn producer_failure_becomes_a_role_failed_event_with_kind_and_phase() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        producer_failed("root", 0, "ProtocolFailure", "provider timed out"),
    ]);

    assert_eq!(
        nodes[0].attempts[0].events,
        vec![AttemptEvent::RoleFailed {
            kind: "ProtocolFailure".to_string(),
            phase: "Producer".to_string(),
            summary: "provider timed out".to_string(),
        }]
    );
}

#[test]
fn validation_failure_captures_command_exit_code_and_caps_combined_lines_at_five() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        validation_failed(
            "cargo test",
            1,
            &["stdout 1", "stdout 2", "stdout 3"],
            &["stderr 1", "stderr 2", "stderr 3", "stderr 4"],
        ),
    ]);

    let AttemptEvent::ValidationFailed {
        command,
        exit_code,
        lines,
    } = &nodes[0].attempts[0].events[0]
    else {
        panic!("expected a ValidationFailed event");
    };
    assert_eq!(command.as_deref(), Some("cargo test"));
    assert_eq!(*exit_code, Some(1));
    assert_eq!(
        lines,
        &vec![
            "stdout 1".to_string(),
            "stdout 2".to_string(),
            "stdout 3".to_string(),
            "stderr 1".to_string(),
            "stderr 2".to_string(),
        ],
        "stdout then stderr, combined and capped at 5 lines total"
    );
}

#[test]
fn work_node_is_accepted_via_integration_succeeded() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        integration_succeeded("root"),
    ]);
    assert_eq!(nodes[0].status, NodeStatus::Accepted);
}

#[test]
fn plan_node_is_accepted_via_plan_accepted_not_integration() {
    let nodes = build(vec![
        run_node("root", 0, "Plan", "decompose the objective"),
        plan_accepted("root"),
    ]);
    assert_eq!(
        nodes[0].status,
        NodeStatus::Accepted,
        "Plan nodes never call IntegrateWork, so PlanAccepted is their accept signal"
    );
}

#[test]
fn node_is_failed_on_terminal_recovery() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        node_failed("root", "ProtocolFailure", "gave up", "Terminal"),
    ]);
    assert_eq!(nodes[0].status, NodeStatus::Failed);
    assert_eq!(nodes[0].last_failure.as_deref(), Some("gave up"));
    assert_eq!(
        nodes[0].attempts[0].events,
        vec![AttemptEvent::RoleFailed {
            kind: "ProtocolFailure".to_string(),
            phase: "Node".to_string(),
            summary: "gave up".to_string(),
        }],
        "the attempt itself must also show the failure inline, independent of node status"
    );
}

#[test]
fn node_is_also_failed_on_retry_recovery_since_a_retry_gets_a_new_node_id() {
    // A retried node is dispatched under a brand-new node id (e.g.
    // "root-retry-1"), never a second RunNode for the same id — so a
    // scheduler that gives up after exhausting retries never emits a
    // `Terminal` recovery for the original id; the last thing recorded for
    // it is a `Retry`. This node's own story still ends here.
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        node_failed("root", "ProviderFailure", "transient error", "Retry"),
    ]);
    assert_eq!(nodes[0].status, NodeStatus::Failed);
    assert_eq!(nodes[0].last_failure.as_deref(), Some("transient error"));
}

#[test]
fn node_failure_summary_takes_only_the_first_line_of_a_multiline_message() {
    // Debug-formatted strings escape embedded newlines as the two-character
    // sequence `\n`, never a raw newline — hence the doubled backslash here,
    // mirroring what a real telemetry file body actually contains.
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        node_failed(
            "root",
            "ValidationFailure",
            "validation failed\\ncommand: cargo test\\nexit code: 1",
            "Retry",
        ),
    ]);
    assert_eq!(
        nodes[0].last_failure.as_deref(),
        Some("validation failed"),
        "embedded newlines in the failure message must not leak into the \
         single-line node summary"
    );
}

#[test]
fn node_with_no_terminal_event_is_unknown() {
    let nodes = build(vec![run_node("root", 0, "Work", "write a function")]);
    assert_eq!(nodes[0].status, NodeStatus::Unknown);
}

#[test]
fn multiple_nodes_preserve_first_seen_order() {
    let nodes = build(vec![
        run_node("root", 0, "Plan", "decompose"),
        plan_accepted("root"),
        run_node("root-child-0", 0, "Work", "first task"),
        integration_succeeded("root-child-0"),
        run_node("root-child-1", 0, "Work", "second task"),
        integration_succeeded("root-child-1"),
    ]);

    let ids: Vec<_> = nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(ids, vec!["root", "root-child-0", "root-child-1"]);
}

#[test]
fn attempts_len_counts_distinct_attempt_numbers_for_a_retried_node() {
    let nodes = build(vec![
        run_node("root", 0, "Work", "write a function"),
        node_failed("root", "ProviderFailure", "transient error", "Retry"),
        run_node("root", 1, "Work", "write a function"),
        integration_succeeded("root"),
    ]);

    assert_eq!(nodes.len(), 1, "a retry reuses the same node id");
    assert_eq!(nodes[0].attempts.len(), 2);
    assert_eq!(nodes[0].attempts[0].attempt, 0);
    assert_eq!(nodes[0].attempts[1].attempt, 1);
    assert_eq!(nodes[0].status, NodeStatus::Accepted);
}
