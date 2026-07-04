//! Node dispatch for scheduler effects.

use crate::artifacts::{Artifact, ArtifactView};
use crate::machines::scheduler::event::SchedulerEvent;
use crate::machines::scheduler::graph::{
    ModelTier, NodeId, NodeKind, RetryFeedback, TestPlanContext,
};
use crate::node_runner::types::NodeRunResult;
use crate::node_runner::{NodeRunRequest, NodeRunner, WorkAttempt};
use crate::telemetry::{ConsoleTelemetry, TelemetrySink};

pub(crate) struct RunNodeDispatch {
    pub(crate) node_id: NodeId,
    pub(crate) worker_role: Option<String>,
    pub(crate) kind: NodeKind,
    pub(crate) objective: String,
    pub(crate) target_files: Vec<String>,
    pub(crate) test_plan_context: TestPlanContext,
    pub(crate) model_tier: ModelTier,
    pub(crate) attempt: u32,
    pub(crate) retry_feedback: Option<RetryFeedback>,
}

pub(crate) struct DispatchResult {
    pub(crate) event: SchedulerEvent,
}

pub(crate) fn dispatch_run_node<R: NodeRunner>(
    runner: &R,
    telemetry: &dyn TelemetrySink,
    command: RunNodeDispatch,
    artifact_snapshot: Option<Artifact>,
    work_attempt: Option<WorkAttempt>,
) -> DispatchResult {
    eprintln!(
        "[scheduler] dispatch {} {:?}",
        command.node_id.short(),
        command.kind
    );

    let artifact_view = artifact_snapshot.as_ref().map(|artifact| ArtifactView {
        repo_path: artifact.repo_path.clone(),
        commit_sha: artifact.commit_sha.clone(),
    });

    let label = progress_label(&command.kind, &command.node_id, &command.worker_role);
    let rendered_objective = render_objective(
        &command.objective,
        &command.target_files,
        command.retry_feedback.as_ref(),
    );
    let request = NodeRunRequest {
        kind: command.kind,
        node_id: command.node_id.clone(),
        objective: rendered_objective,
        target_files: command.target_files,
        test_plan_context: command.test_plan_context,
        model_tier: command.model_tier,
        attempt: command.attempt,
        artifact_view,
        work_attempt,
    };
    let console_tel = ConsoleTelemetry::new(telemetry, label);
    let result = runner.run_node(request, &console_tel);
    DispatchResult {
        event: node_result_event(command.node_id, result),
    }
}

/// Builds the `[planner ...]`/`[worker ...]` progress-line label for a
/// dispatched node, appending the short node id and (for Work nodes) the
/// worker role when the node has one.
fn progress_label(kind: &NodeKind, node_id: &NodeId, worker_role: &Option<String>) -> String {
    match kind {
        NodeKind::Plan => format!("[planner {}]", node_id.short()),
        NodeKind::Work => match worker_role {
            Some(role) => format!("[worker {}/{role}]", node_id.short()),
            None => format!("[worker {}]", node_id.short()),
        },
    }
}

fn node_result_event(node_id: NodeId, result: NodeRunResult) -> SchedulerEvent {
    match result {
        NodeRunResult::PlanAccepted(plan) => SchedulerEvent::PlanAccepted { node_id, plan },
        NodeRunResult::WorkAccepted(work_result) => SchedulerEvent::WorkAccepted {
            node_id,
            work: work_result.work,
        },
        NodeRunResult::Failed(failure) => SchedulerEvent::NodeFailed { node_id, failure },
    }
}

/// Renders the prompt objective, appending validation feedback when present.
///
/// The machine stores `retry_feedback` separately so the objective field
/// remains the original task description. This function combines them at
/// dispatch time so the runner receives the full context.
fn render_objective(
    objective: &str,
    target_files: &[String],
    retry_feedback: Option<&RetryFeedback>,
) -> String {
    let Some(feedback) = retry_feedback else {
        return objective.to_string();
    };
    let target_text = if target_files.is_empty() {
        "(none specified)".to_string()
    } else {
        target_files.join(", ")
    };
    format!(
        "{objective}\n\nValidation feedback for retry:\nTarget files: {target_text}\n{}",
        feedback.diagnostics
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── progress_label ────────────────────────────────────────────────────

    #[test]
    fn plan_node_label_ignores_worker_role() {
        // Invariant: Plan nodes always render as "[planner <short id>]",
        // regardless of worker_role (which is always None for Plan nodes,
        // but the label must not depend on that being true).
        let label = progress_label(&NodeKind::Plan, &NodeId("root".to_string()), &None);
        assert_eq!(
            label,
            format!("[planner {}]", NodeId("root".to_string()).short())
        );
    }

    #[test]
    fn work_node_label_appends_worker_role_when_present() {
        let label = progress_label(
            &NodeKind::Work,
            &NodeId("a3f7c2".to_string()),
            &Some("tester".to_string()),
        );
        assert_eq!(label, "[worker a3f7c2/tester]");
    }

    #[test]
    fn work_node_label_omits_slash_when_worker_role_absent() {
        let label = progress_label(&NodeKind::Work, &NodeId("a3f7c2".to_string()), &None);
        assert_eq!(label, "[worker a3f7c2]");
    }
}
