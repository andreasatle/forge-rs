//! Conversion from deliberation terminal output into node-run results.

use crate::machines::deliberation::DeliberationTerminalOutput;
use crate::machines::scheduler::{FailureKind, NodeFailure, NodeKind, RecoveryAction, WorkOutput};
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

use crate::node_runner::classify::{classify_deliberation_failure, recovery_label};
use crate::node_runner::types::{NodeRunResult, NodeRunWorkResult};

pub(crate) fn map_output(
    output: DeliberationTerminalOutput,
    kind: NodeKind,
    telemetry: &dyn TelemetrySink,
) -> NodeRunResult {
    match output {
        DeliberationTerminalOutput::Complete(out) => match kind {
            NodeKind::Plan => map_plan_output(out.content, telemetry),
            NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: out.content,
                },
            }),
        },
        DeliberationTerminalOutput::Failed { kind, message, .. } => {
            let recovery = classify_deliberation_failure(kind, &message);
            telemetry.record(TelemetryRecord::new(
                "DeliberatingNodeRunner",
                TelemetryEvent::FailureClassified {
                    reason: message.clone(),
                    recovery: recovery_label(&recovery).to_string(),
                },
            ));
            NodeRunResult::Failed(NodeFailure {
                kind,
                message,
                recovery,
            })
        }
    }
}

/// Map a plan node's raw content to a [`NodeRunResult`].
///
/// Attempts to parse `content` as a structured [`PlannerOutput`] JSON object.
///
/// - If parsing succeeds and the graph is structurally valid: emits
///   `PlannerOutputParsed` and returns `PlanAccepted` with one `NodeRequest`
///   per task. No-recreate validation has already been enforced by the handler
///   before the content reaches this function.
/// - If parsing succeeds but structural validation fails: emits
///   `PlannerOutputValidationFailed` and returns `Failed` with `Terminal`
///   recovery.
/// - If parsing fails (prose or unexpected schema): emits
///   `PlannerOutputFallback` and returns `Failed` with `Terminal` recovery.
fn map_plan_output(content: String, telemetry: &dyn TelemetrySink) -> NodeRunResult {
    use crate::node_runner::planner::PlannerOutputProcessor;

    match PlannerOutputProcessor::parse_content(&content) {
        Some(planner_out) => match PlannerOutputProcessor::validate_structure(&planner_out) {
            Ok(()) => {
                let task_count = planner_out.tasks.len();
                let dependency_count: usize =
                    planner_out.tasks.iter().map(|t| t.depends_on.len()).sum();
                telemetry.record(TelemetryRecord::new(
                    "DeliberatingNodeRunner",
                    TelemetryEvent::PlannerOutputParsed {
                        task_count,
                        dependency_count,
                    },
                ));
                NodeRunResult::PlanAccepted(PlannerOutputProcessor::into_plan_output(planner_out))
            }
            Err(e) => {
                let reason = e.to_string();
                telemetry.record(TelemetryRecord::new(
                    "DeliberatingNodeRunner",
                    TelemetryEvent::PlannerOutputValidationFailed {
                        reason: reason.clone(),
                    },
                ));
                NodeRunResult::Failed(NodeFailure {
                    kind: FailureKind::PlannerValidationFailure,
                    message: reason.clone(),
                    recovery: RecoveryAction::Terminal {
                        message: format!("planner output validation failed: {reason}"),
                    },
                })
            }
        },
        None => {
            // Planner content was not valid PlannerOutput JSON.
            // This path should be unreachable when runner validation is active,
            // but if reached it must fail loudly rather than silently substituting
            // a single work node.
            let reason = "planner content is not valid PlannerOutput JSON".to_string();
            telemetry.record(TelemetryRecord::new(
                "DeliberatingNodeRunner",
                TelemetryEvent::PlannerOutputFallback,
            ));
            NodeRunResult::Failed(NodeFailure {
                kind: FailureKind::PlannerValidationFailure,
                message: reason.clone(),
                recovery: RecoveryAction::Terminal {
                    message: format!("planner output invalid: {reason}"),
                },
            })
        }
    }
}
