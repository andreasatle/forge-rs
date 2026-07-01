//! Pure text rendering of the node/attempt-grouped default view.
//!
//! Kept string-in-string-out (no file I/O, no `println!`) so the format can
//! be tested directly against hand-built fixtures.

use super::super::reader::truncate;
use super::{AttemptEvent, AttemptSummary, NodeStatus, NodeSummary};

/// Rationale/reason text longer than this is truncated with an ellipsis.
const RATIONALE_MAX_CHARS: usize = 120;
/// Node-list `last:` failure text is truncated to match the flat summary's
/// preview width.
const SUMMARY_MAX_CHARS: usize = 80;

pub(super) fn render(
    run_id: &str,
    objective: Option<&str>,
    event_count: usize,
    nodes: &[NodeSummary],
) -> String {
    let mut out = String::new();

    out.push_str(&format!("run_id: {run_id}\n"));
    out.push_str(&format!(
        "objective: {}\n",
        objective.unwrap_or("(unknown)")
    ));
    out.push_str(&format!("event_count: {event_count}\n"));
    out.push('\n');

    out.push_str("nodes:\n");
    for node in nodes {
        out.push_str(&format!("  {}\n", node_list_line(node)));
    }
    out.push('\n');

    out.push_str("timeline:\n");
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_node(&mut out, node);
    }

    out.trim_end_matches('\n').to_string()
}

fn node_list_line(node: &NodeSummary) -> String {
    let mut line = format!(
        "{}  kind={}  status={}  attempts={}",
        node.node_id,
        node.kind.as_deref().unwrap_or("unknown"),
        status_word(&node.status),
        node.attempts.len()
    );
    if node.status == NodeStatus::Failed
        && let Some(last) = &node.last_failure
    {
        line.push_str(&format!("  [last: {}]", truncate(last, SUMMARY_MAX_CHARS)));
    }
    line
}

fn status_word(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Accepted => "accepted",
        NodeStatus::Failed => "failed",
        NodeStatus::Unknown => "unknown",
    }
}

fn render_node(out: &mut String, node: &NodeSummary) {
    out.push_str(&format!("node {}:\n", node.node_id));
    out.push_str("  objective:\n");
    out.push_str(&format!(
        "    {}\n",
        node.objective.as_deref().unwrap_or("(unknown)")
    ));

    for attempt in &node.attempts {
        out.push('\n');
        render_attempt(out, attempt);
    }
}

fn render_attempt(out: &mut String, attempt: &AttemptSummary) {
    out.push_str(&format!("  attempt {}:\n", attempt.attempt));
    for event in &attempt.events {
        render_event(out, event);
    }
}

fn render_event(out: &mut String, event: &AttemptEvent) {
    match event {
        AttemptEvent::Producer {
            completed,
            tool_calls,
        } => {
            let word = if *completed { "completed" } else { "retry" };
            out.push_str(&format!("    producer: {word} [tool_calls={tool_calls}]\n"));
        }
        AttemptEvent::Validator { accepted, reason } => {
            let word = if *accepted { "accepted" } else { "rejected" };
            match reason {
                Some(reason) => {
                    out.push_str(&format!("    validator: {word} [reason: {reason}]\n"))
                }
                None => out.push_str(&format!("    validator: {word}\n")),
            }
        }
        AttemptEvent::Critic {
            accepted,
            rationale,
        } => {
            render_review(out, "critic", *accepted, rationale);
        }
        AttemptEvent::Referee {
            accepted,
            rationale,
        } => {
            render_review(out, "referee", *accepted, rationale);
        }
        AttemptEvent::RoleFailed {
            kind,
            phase,
            summary,
        } => {
            out.push_str(&format!(
                "    failed: {kind} {phase} {}\n",
                truncate(summary, SUMMARY_MAX_CHARS)
            ));
        }
        AttemptEvent::ValidationFailed {
            command,
            exit_code,
            lines,
        } => {
            let command = command.as_deref().unwrap_or("(unknown)");
            let exit_code = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "none".to_string());
            out.push_str(&format!(
                "    validation failed: {command} exit={exit_code}\n"
            ));
            for line in lines {
                out.push_str(&format!("      {line}\n"));
            }
        }
    }
}

fn render_review(out: &mut String, role: &str, accepted: bool, rationale: &str) {
    let word = if accepted { "accept" } else { "reject" };
    out.push_str(&format!(
        "    {role}: {word} {}\n",
        truncate(rationale, RATIONALE_MAX_CHARS)
    ));
}
