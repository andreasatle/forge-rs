//! Turns a node/attempt-contextualized telemetry stream into
//! `Vec<NodeSummary>`.

use super::parsing::{ContextRecord, RawRecord, debug_field, event_variant_name};
use super::{AttemptEvent, AttemptSummary, NodeStatus, NodeSummary};

/// Accumulates one node's fields while walking the record stream. Carries
/// transient round-tracking state (`producer_rounds`/`producer_tool_calls`
/// per attempt) that doesn't belong in the public [`NodeSummary`]/
/// [`AttemptSummary`] types, so it's finalized into them once the whole
/// stream has been walked.
struct NodeBuilder {
    node_id: String,
    kind: Option<String>,
    objective: Option<String>,
    status: NodeStatus,
    last_failure: Option<String>,
    attempts: Vec<AttemptBuilder>,
}

struct AttemptBuilder {
    attempt: u32,
    events: Vec<AttemptEvent>,
    producer_rounds: u32,
    producer_tool_calls: u32,
}

/// Group `records` into one [`NodeSummary`] per distinct node id, in
/// first-seen order, each holding one [`AttemptSummary`] per distinct
/// attempt number, in first-seen order.
pub(super) fn group_into_nodes(records: Vec<ContextRecord>) -> Vec<NodeSummary> {
    let mut nodes: Vec<NodeBuilder> = Vec::new();

    for record in records {
        let node = match nodes.iter().position(|n| n.node_id == record.node_id) {
            Some(idx) => &mut nodes[idx],
            None => {
                nodes.push(NodeBuilder {
                    node_id: record.node_id.clone(),
                    kind: None,
                    objective: None,
                    status: NodeStatus::Unknown,
                    last_failure: None,
                    attempts: Vec::new(),
                });
                nodes.last_mut().unwrap()
            }
        };
        apply_node_level(node, &record.record);

        let attempt = match node
            .attempts
            .iter()
            .position(|a| a.attempt == record.attempt)
        {
            Some(idx) => &mut node.attempts[idx],
            None => {
                node.attempts.push(AttemptBuilder {
                    attempt: record.attempt,
                    events: Vec::new(),
                    producer_rounds: 0,
                    producer_tool_calls: 0,
                });
                node.attempts.last_mut().unwrap()
            }
        };
        apply_attempt_level(attempt, &record.record);
    }

    nodes
        .into_iter()
        .map(|n| NodeSummary {
            node_id: n.node_id,
            kind: n.kind,
            objective: n.objective,
            status: n.status,
            last_failure: n.last_failure,
            attempts: n
                .attempts
                .into_iter()
                .map(|a| AttemptSummary {
                    attempt: a.attempt,
                    events: a.events,
                })
                .collect(),
        })
        .collect()
}

/// Update node-wide fields (`kind`, `objective`, terminal `status`) from one
/// record. These aren't attempt-scoped: `kind`/`objective` come from the
/// `RunNode` dispatch (constant across retries), and node status is decided
/// by whichever attempt last reports a terminal outcome.
///
/// A retried node gets a brand-new node id (e.g. `root-child-1-retry-2`),
/// never a repeat dispatch of the same id — so `NodeFailed`/`IntegrationFailed`
/// always ends *this* node's story, regardless of `recovery` kind ("exhausted"
/// runs never emit a `Terminal` recovery; the scheduler just stops retrying).
fn apply_node_level(node: &mut NodeBuilder, record: &RawRecord) {
    let body = &record.body;

    if record.source == "SchedulerMachine" && record.kind == "EffectEmitted" {
        if event_variant_name(body) == Some("RunNode") {
            if node.kind.is_none() {
                node.kind = debug_field(body, "kind");
            }
            if node.objective.is_none() {
                node.objective = debug_field(body, "objective");
            }
        }
        return;
    }

    if record.source != "SchedulerMachine" || record.kind != "EventReceived" {
        return;
    }

    match event_variant_name(body) {
        Some("IntegrationSucceeded") | Some("PlanAccepted") => {
            node.status = NodeStatus::Accepted;
            node.last_failure = None;
        }
        Some("NodeFailed") | Some("IntegrationFailed") => {
            node.status = NodeStatus::Failed;
            node.last_failure = debug_field(body, "message").map(|m| first_line(&m));
        }
        _ => {}
    }
}

/// Walk one attempt's records in order, turning role-protocol and
/// validation events into [`AttemptEvent`]s.
fn apply_attempt_level(attempt: &mut AttemptBuilder, record: &RawRecord) {
    let body = &record.body;

    if record.source == "RoleMachine" && record.subsource.as_deref() == Some("Producer") {
        match record.kind.as_str() {
            "ToolRequested" => attempt.producer_tool_calls += 1,
            "ParseSucceeded" => {
                let completed = attempt.producer_rounds == 0;
                attempt.producer_rounds += 1;
                let tool_calls = attempt.producer_tool_calls;
                attempt.producer_tool_calls = 0;
                attempt.events.push(AttemptEvent::Producer {
                    completed,
                    tool_calls,
                });
            }
            _ => {}
        }
        return;
    }

    if record.source == "DeliberationMachine" && record.kind == "EventReceived" {
        match event_variant_name(body) {
            Some("ProducerValidationAccepted") => {
                attempt.events.push(AttemptEvent::Validator {
                    accepted: true,
                    reason: None,
                });
            }
            Some("ProducerValidationRejected") => {
                let reason = debug_field(body, "feedback_reason").map(|r| first_line(&r));
                attempt.events.push(AttemptEvent::Validator {
                    accepted: false,
                    reason,
                });
            }
            Some("CriticAccepted") => {
                let rationale = debug_field(body, "content").unwrap_or_default();
                attempt.events.push(AttemptEvent::Critic {
                    accepted: true,
                    rationale,
                });
            }
            Some("CriticRejected") => {
                let rationale = debug_field(body, "reason").unwrap_or_default();
                attempt.events.push(AttemptEvent::Critic {
                    accepted: false,
                    rationale,
                });
            }
            Some("RefereeAccepted") => {
                let rationale = debug_field(body, "content").unwrap_or_default();
                attempt.events.push(AttemptEvent::Referee {
                    accepted: true,
                    rationale,
                });
            }
            Some("RefereeRejected") => {
                let rationale = debug_field(body, "reason").unwrap_or_default();
                attempt.events.push(AttemptEvent::Referee {
                    accepted: false,
                    rationale,
                });
            }
            Some(variant @ ("ProducerFailed" | "CriticFailed" | "RefereeFailed")) => {
                let phase = variant.trim_end_matches("Failed").to_string();
                let kind = debug_field(body, "kind").unwrap_or_default();
                let summary = debug_field(body, "reason")
                    .map(|r| first_line(&r))
                    .unwrap_or_default();
                attempt.events.push(AttemptEvent::RoleFailed {
                    kind,
                    phase,
                    summary,
                });
            }
            _ => {}
        }
        return;
    }

    if record.source == "SchedulerMachine" && record.kind == "EventReceived" {
        if let Some(variant @ ("NodeFailed" | "IntegrationFailed")) = event_variant_name(body) {
            let kind = debug_field(body, "kind").unwrap_or_default();
            let summary = debug_field(body, "message")
                .map(|m| first_line(&m))
                .unwrap_or_default();
            let phase = if variant == "NodeFailed" {
                "Node"
            } else {
                "Integration"
            }
            .to_string();
            attempt.events.push(AttemptEvent::RoleFailed {
                kind,
                phase,
                summary,
            });
        }
        return;
    }

    if record.source == "Integration" && record.kind == "ValidationFailed" {
        let (command, exit_code, lines) = parse_validation_failed(body);
        attempt.events.push(AttemptEvent::ValidationFailed {
            command,
            exit_code,
            lines,
        });
    }
}

const STDOUT_MARKER: &str = "stdout:\n";
const STDERR_MARKER: &str = "stderr:\n";
const MAX_VALIDATION_LINES: usize = 5;

fn parse_validation_failed(body: &str) -> (Option<String>, Option<i32>, Vec<String>) {
    let command = body
        .lines()
        .find_map(|l| l.strip_prefix("command: "))
        .map(str::to_string);
    let exit_code = body
        .lines()
        .find_map(|l| l.strip_prefix("exit_code: "))
        .and_then(|v| v.parse().ok());

    let stdout_start = body
        .find(STDOUT_MARKER)
        .map(|idx| idx + STDOUT_MARKER.len());
    let stderr_idx = body.find(STDERR_MARKER);
    let stdout = match (stdout_start, stderr_idx) {
        (Some(start), Some(end)) if end >= start => &body[start..end],
        (Some(start), None) => &body[start..],
        _ => "",
    };
    let stderr = stderr_idx
        .map(|idx| &body[idx + STDERR_MARKER.len()..])
        .unwrap_or("");

    let mut lines: Vec<String> = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    lines.truncate(MAX_VALIDATION_LINES);

    (command, exit_code, lines)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}
