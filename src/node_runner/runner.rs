//! The NodeRunner trait and the static fake implementation.

use crate::machines::scheduler::{
    NodeFailure, NodeId, NodeKind, NodeOutcome, NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
};

use super::types::{NodeRunRequest, NodeRunResult};

/// Runs a single scheduler node and returns a typed outcome.
///
/// Implementations may call providers, tools, or other I/O. The trait itself
/// is synchronous; async integration is a later concern.
pub trait NodeRunner {
    /// Execute `request` and return the outcome.
    fn run_node(&self, request: NodeRunRequest) -> NodeRunResult;
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
    fn run_node(&self, request: NodeRunRequest) -> NodeRunResult {
        if request.objective.contains("fail") {
            return NodeRunResult::Failed(NodeFailure {
                reason: "objective contains 'fail'".to_string(),
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
                    dependencies: vec![],
                }],
            }),
            NodeKind::Work => NodeRunResult::WorkAccepted(WorkOutput {
                summary: format!("completed: {}", request.objective),
            }),
        }
    }
}

impl From<NodeRunResult> for NodeOutcome {
    fn from(result: NodeRunResult) -> Self {
        match result {
            NodeRunResult::PlanAccepted(plan) => NodeOutcome::PlanAccepted(plan),
            NodeRunResult::WorkAccepted(work) => NodeOutcome::WorkAccepted(work),
            NodeRunResult::Failed(failure) => NodeOutcome::Failed(failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::scheduler::{ModelTier, RecoveryAction};

    fn plan_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Plan,
            objective: objective.to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        }
    }

    fn work_request(objective: &str) -> NodeRunRequest {
        NodeRunRequest {
            kind: NodeKind::Work,
            objective: objective.to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        }
    }

    #[test]
    fn node_run_request_carries_scheduler_fields() {
        let req = NodeRunRequest {
            kind: NodeKind::Work,
            objective: "test objective".to_string(),
            model_tier: ModelTier::Strong,
            attempt: 2,
        };
        assert_eq!(req.kind, NodeKind::Work);
        assert_eq!(req.objective, "test objective");
        assert_eq!(req.model_tier, ModelTier::Strong);
        assert_eq!(req.attempt, 2);
    }

    #[test]
    fn static_runner_plan_returns_plan_accepted() {
        let result = StaticNodeRunner.run_node(plan_request("build the thing"));
        assert!(matches!(result, NodeRunResult::PlanAccepted(_)));
        let NodeRunResult::PlanAccepted(plan) = result else {
            unreachable!()
        };
        assert_eq!(plan.children.len(), 1);
        assert_eq!(plan.children[0].kind, NodeKind::Work);
    }

    #[test]
    fn static_runner_work_returns_work_accepted() {
        let result = StaticNodeRunner.run_node(work_request("write some code"));
        assert!(matches!(result, NodeRunResult::WorkAccepted(_)));
        let NodeRunResult::WorkAccepted(work) = result else {
            unreachable!()
        };
        assert!(work.summary.contains("write some code"));
    }

    #[test]
    fn static_runner_fail_returns_node_failure() {
        let result = StaticNodeRunner.run_node(work_request("do a failing task"));
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

        let work_result = NodeRunResult::WorkAccepted(WorkOutput {
            summary: "done".to_string(),
        });
        assert!(matches!(
            NodeOutcome::from(work_result),
            NodeOutcome::WorkAccepted(_)
        ));

        let fail_result = NodeRunResult::Failed(NodeFailure {
            reason: "bad".to_string(),
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
