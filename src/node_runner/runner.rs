//! The NodeRunner trait and the static fake implementation.

use crate::machines::scheduler::{
    FailureKind, NodeFailure, NodeId, NodeKind, NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
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
            NodeKind::OldDecomposition | NodeKind::Plan => {
                NodeRunResult::PlanAccepted(PlanOutput {
                    children: vec![NodeRequest {
                        id: NodeId("child-work".to_string()),
                        kind: NodeKind::Work,
                        worker_role: None,
                        objective: format!("work for: {}", request.objective),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    }],
                })
            }
            NodeKind::Work => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: format!("completed: {}", request.objective),
                },
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::scheduler::{ModelTier, RecoveryAction, TestPlanContext};
    use crate::telemetry::NoopTelemetry;

    fn plan_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Plan,
            node_id: NodeId("test-node".to_string()),
            objective: objective.to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            worker_role: None,
            work_attempt: None,
        }
    }

    fn work_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Work,
            node_id: NodeId("test-node".to_string()),
            objective: objective.to_string(),
            target_files: vec![],
            test_plan_context: TestPlanContext::default(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
            artifact_view: None,
            worker_role: None,
            work_attempt: None,
        }
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
}
