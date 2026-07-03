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

pub(super) struct DefaultTraceRenderer<'a> {
    out: String,
    run_id: &'a str,
    objective: Option<&'a str>,
    event_count: usize,
    nodes: &'a [NodeSummary],
}

impl<'a> DefaultTraceRenderer<'a> {
    pub(super) fn new(
        run_id: &'a str,
        objective: Option<&'a str>,
        event_count: usize,
        nodes: &'a [NodeSummary],
    ) -> Self {
        Self {
            out: String::new(),
            run_id,
            objective,
            event_count,
            nodes,
        }
    }

    pub(super) fn render(mut self) -> String {
        self.render_header();
        self.render_node_list();
        self.render_timeline();
        self.out.trim_end_matches('\n').to_string()
    }

    fn render_header(&mut self) {
        self.out.push_str(&format!("run_id: {}\n", self.run_id));
        self.out.push_str(&format!(
            "objective: {}\n",
            self.objective.unwrap_or("(unknown)")
        ));
        self.out
            .push_str(&format!("event_count: {}\n", self.event_count));
        self.out.push('\n');
    }

    fn render_node_list(&mut self) {
        self.out.push_str("nodes:\n");
        for node in self.nodes {
            self.out
                .push_str(&format!("  {}\n", Self::node_list_line(node)));
        }
        self.out.push('\n');
    }

    fn render_timeline(&mut self) {
        self.out.push_str("timeline:\n");
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            self.render_node(node);
        }
    }

    fn node_list_line(node: &NodeSummary) -> String {
        let mut line = format!(
            "{}  kind={}  status={}  attempts={}",
            node.node_id,
            node.kind.as_deref().unwrap_or("unknown"),
            Self::status_word(&node.status),
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

    fn render_node(&mut self, node: &NodeSummary) {
        self.out.push_str(&format!("node {}:\n", node.node_id));
        self.out.push_str("  objective:\n");
        self.out.push_str(&format!(
            "    {}\n",
            node.objective.as_deref().unwrap_or("(unknown)")
        ));

        for attempt in &node.attempts {
            self.out.push('\n');
            self.render_attempt(attempt);
        }
    }

    fn render_attempt(&mut self, attempt: &AttemptSummary) {
        self.out
            .push_str(&format!("  attempt {}:\n", attempt.attempt));
        for event in &attempt.events {
            self.render_event(event);
        }
    }

    fn render_event(&mut self, event: &AttemptEvent) {
        match event {
            AttemptEvent::Producer {
                completed,
                tool_calls,
            } => {
                let word = if *completed { "completed" } else { "retry" };
                self.out
                    .push_str(&format!("    producer: {word} [tool_calls={tool_calls}]\n"));
            }
            AttemptEvent::Validator { accepted, reason } => {
                let word = if *accepted { "accepted" } else { "rejected" };
                match reason {
                    Some(reason) => self
                        .out
                        .push_str(&format!("    validator: {word} [reason: {reason}]\n")),
                    None => self.out.push_str(&format!("    validator: {word}\n")),
                }
            }
            AttemptEvent::Critic {
                accepted,
                rationale,
            } => {
                self.render_review("critic", *accepted, rationale);
            }
            AttemptEvent::Referee {
                accepted,
                rationale,
            } => {
                self.render_review("referee", *accepted, rationale);
            }
            AttemptEvent::RoleFailed {
                kind,
                phase,
                summary,
            } => {
                self.out.push_str(&format!(
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
                self.out.push_str(&format!(
                    "    validation failed: {command} exit={exit_code}\n"
                ));
                for line in lines {
                    self.out.push_str(&format!("      {line}\n"));
                }
            }
        }
    }

    fn render_review(&mut self, role: &str, accepted: bool, rationale: &str) {
        let word = if accepted { "accept" } else { "reject" };
        self.out.push_str(&format!(
            "    {role}: {word} {}\n",
            truncate(rationale, RATIONALE_MAX_CHARS)
        ));
    }
}
