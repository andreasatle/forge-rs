//! Node dispatch for scheduler effects.

use crate::artifacts::{Artifact, ArtifactUpdate, ArtifactView};
use crate::machines::scheduler::event::{NodeOutcome, SchedulerEvent};
use crate::machines::scheduler::state::{ModelTier, NodeId, NodeKind, TestPlanContext};
use crate::node_runner::{NodeRunRequest, NodeRunResult, NodeRunner, WorkAttempt};
use crate::telemetry::{ConsoleTelemetry, TelemetrySink};

pub(crate) struct RunNodeDispatch {
    pub(crate) node_id: NodeId,
    pub(crate) kind: NodeKind,
    pub(crate) objective: String,
    pub(crate) target_files: Vec<String>,
    pub(crate) test_plan_context: TestPlanContext,
    pub(crate) model_tier: ModelTier,
    pub(crate) attempt: u32,
}

pub(crate) struct DispatchResult {
    pub(crate) event: SchedulerEvent,
    pub(crate) artifact_update: Option<ArtifactUpdate>,
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
        command.node_id.0, command.kind
    );

    let artifact_view = artifact_snapshot.as_ref().map(|artifact| ArtifactView {
        repo_path: artifact.repo_path.clone(),
        commit_sha: artifact.commit_sha.clone(),
    });

    let label = match &command.kind {
        NodeKind::Plan => "[planner]".to_string(),
        NodeKind::Work => format!("[worker {}]", command.node_id.0),
    };
    let request = NodeRunRequest {
        kind: command.kind,
        objective: command.objective,
        target_files: command.target_files,
        test_plan_context: command.test_plan_context,
        model_tier: command.model_tier,
        attempt: command.attempt,
        artifact_view,
        work_attempt,
    };
    let console_tel = ConsoleTelemetry::new(telemetry, label);
    let result = runner.run_node(request, &console_tel);
    let artifact_update = match &result {
        NodeRunResult::WorkAccepted(work_result) => work_result.artifact_update.clone(),
        _ => None,
    };

    DispatchResult {
        event: SchedulerEvent::NodeReturned {
            node_id: command.node_id,
            outcome: NodeOutcome::from(result),
        },
        artifact_update,
    }
}
