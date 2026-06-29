//! The NodeRunner trait and the static fake implementation.

use crate::machines::scheduler::{
    FailureKind, NodeFailure, NodeId, NodeKind, NodeOutcome, NodeRequest, PlanOutput,
    RecoveryAction, WorkOutput,
};
use crate::telemetry::TelemetrySink;

use super::types::{NodeRunRequest, NodeRunResult, NodeRunWorkResult};

/// Runs a single scheduler node and returns a typed outcome.
///
/// Implementations may call providers, tools, or other I/O. The trait itself
/// is synchronous; async integration is a later concern.
pub trait NodeRunner {
    /// Execute `request` and return the outcome, recording into the shared `telemetry` sink.
    fn run_node(&self, request: NodeRunRequest, telemetry: &dyn TelemetrySink) -> NodeRunResult;
}

/// A minimal fake runner for tests and early development.
///
/// Outcomes are determined by the request fields alone — no I/O, no providers.
///
/// Rules:
/// - If `objective` contains `"fail"`, return `Failed` with `Terminal` recovery.
/// - If `kind` is `Plan`, return `PlanAccepted` with one child work node.
/// - If `kind` is `Work`, return `WorkAccepted` with a summary derived from the objective.
pub struct StaticNodeRunner;

impl NodeRunner for StaticNodeRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        if request.objective.contains("fail") {
            return NodeRunResult::Failed(NodeFailure {
                kind: FailureKind::UserTaskRejection,
                message: "objective contains 'fail'".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "static runner: terminal failure".to_string(),
                },
            });
        }
        match request.kind {
            NodeKind::Plan => NodeRunResult::PlanAccepted(PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("child-work".to_string()),
                    kind: NodeKind::Work,
                    objective: format!("work for: {}", request.objective),
                    target_files: vec![],
                    required_test_targets: vec![],
                    dependencies: vec![],
                    validation_plan: None,
                }],
            }),
            NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: format!("completed: {}", request.objective),
                },
            }),
        }
    }
}

impl From<NodeRunResult> for NodeOutcome {
    fn from(result: NodeRunResult) -> Self {
        match result {
            NodeRunResult::PlanAccepted(plan) => NodeOutcome::PlanAccepted(plan),
            NodeRunResult::WorkAccepted(work_result) => NodeOutcome::WorkAccepted(work_result.work),
            NodeRunResult::Failed(failure) => NodeOutcome::Failed(failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::artifacts::ArtifactView;
    use crate::machines::scheduler::{ModelTier, RecoveryAction, TestPlanContext};
    use crate::telemetry::NoopTelemetry;

    fn plan_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: objective.to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            work_attempt: None,
        }
    }

    fn work_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Work,
            objective: objective.to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            work_attempt: None,
        }
    }

    #[test]
    fn node_run_request_carries_scheduler_fields() {
        let req = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "test objective".to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Strong,
            attempt: 2,
            artifact_view: None,
            work_attempt: None,
        };
        assert_eq!(req.kind, NodeKind::Work);
        assert_eq!(req.objective, "test objective");
        assert_eq!(req.model_tier, ModelTier::Strong);
        assert_eq!(req.attempt, 2);
    }

    #[test]
    fn node_run_request_can_carry_artifact_view() {
        let view = ArtifactView {
            repo_path: PathBuf::from("/some/repo.git"),
            commit_sha: "deadbeef".to_string(),
        };
        let req = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "test".to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: Some(view),
            work_attempt: None,
        };
        let stored = req.artifact_view.expect("artifact_view should be Some");
        assert_eq!(stored.commit_sha, "deadbeef");
    }

    #[test]
    fn static_runner_plan_returns_plan_accepted() {
        let result = StaticNodeRunner.run_node(plan_request("build the thing"), &NoopTelemetry);
        assert!(matches!(result, NodeRunResult::PlanAccepted(_)));
        let NodeRunResult::PlanAccepted(plan) = result else {
            unreachable!()
        };
        assert_eq!(plan.children.len(), 1);
        assert_eq!(plan.children[0].kind, NodeKind::Work);
    }

    #[test]
    fn static_runner_work_returns_work_accepted() {
        let result = StaticNodeRunner.run_node(work_request("write some code"), &NoopTelemetry);
        let NodeRunResult::WorkAccepted(work_result) = result else {
            panic!("expected WorkAccepted");
        };
        assert!(work_result.work.summary.contains("write some code"));
    }

    #[test]
    fn static_runner_fail_returns_node_failure() {
        let result = StaticNodeRunner.run_node(work_request("do a failing task"), &NoopTelemetry);
        assert!(matches!(result, NodeRunResult::Failed(_)));
        let NodeRunResult::Failed(failure) = result else {
            unreachable!()
        };
        assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));
    }

    #[test]
    fn node_run_result_can_convert_to_node_outcome() {
        let plan_result = NodeRunResult::PlanAccepted(PlanOutput { children: vec![] });
        assert!(matches!(
            NodeOutcome::from(plan_result),
            NodeOutcome::PlanAccepted(_)
        ));

        let work_result = NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: "done".to_string(),
            },
        });
        assert!(matches!(
            NodeOutcome::from(work_result),
            NodeOutcome::WorkAccepted(_)
        ));

        let fail_result = NodeRunResult::Failed(NodeFailure {
            kind: FailureKind::DeliberationFailure,
            message: "bad".to_string(),
            recovery: RecoveryAction::Terminal {
                message: "stop".to_string(),
            },
        });
        assert!(matches!(
            NodeOutcome::from(fail_result),
            NodeOutcome::Failed(_)
        ));
    }
}
