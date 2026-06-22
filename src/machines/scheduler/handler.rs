//! Effect handler for the scheduler machine.
//!
//! [`SchedulerHandler`] implements [`Machine`] by delegating pure transition
//! logic to [`SchedulerMachine`] and forwarding [`SchedulerEffect::RunNode`]
//! effects to a [`NodeRunner`].
//!
//! The scheduler itself does not know how node outcomes are produced. All fake
//! or real execution responsibility belongs here, behind the [`NodeRunner`]
//! boundary.

use crate::engine::{Machine, Transition};
use crate::machines::scheduler::effect::SchedulerEffect;
use crate::machines::scheduler::event::{
    IntegrationOutcome, IntegrationOutput, NodeOutcome, SchedulerEvent,
};
use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
use crate::machines::scheduler::state::SchedulerState;
use crate::node_runner::{NodeRunRequest, NodeRunner};

/// Drives the scheduler machine using a [`NodeRunner`] to execute nodes.
///
/// All pure transition logic is delegated to [`SchedulerMachine`]. This type
/// owns only effect execution: converting a `RunNode` effect into a runner
/// call and translating the result back into a `NodeReturned` event.
pub struct SchedulerHandler<R> {
    runner: R,
}

impl<R: NodeRunner> SchedulerHandler<R> {
    /// Create a new handler backed by the given [`NodeRunner`].
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: NodeRunner> Machine for SchedulerHandler<R> {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerOutput;

    fn start_event(&self) -> SchedulerEvent {
        SchedulerMachine.start_event()
    }

    fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        SchedulerMachine.transition(state, event)
    }

    fn handle_effect(&self, effect: SchedulerEffect) -> SchedulerEvent {
        match effect {
            SchedulerEffect::RunNode {
                node_id,
                kind,
                objective,
                model_tier,
                attempt,
            } => {
                let request = NodeRunRequest {
                    kind,
                    objective,
                    model_tier,
                    attempt,
                    // The scheduler does not supply an artifact view yet.
                    artifact_view: None,
                };
                let result = self.runner.run_node(request);
                SchedulerEvent::NodeReturned {
                    node_id,
                    outcome: NodeOutcome::from(result),
                }
            }

            SchedulerEffect::IntegrateWork { node_id, work } => {
                SchedulerEvent::IntegrationReturned {
                    node_id,
                    outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                        summary: work.summary,
                    }),
                }
            }

            SchedulerEffect::ReturnComplete { .. } | SchedulerEffect::ReturnFailed { .. } => {
                unreachable!("return effects are never dispatched to the effect handler")
            }
        }
    }

    fn output(&self, state: &SchedulerState) -> Option<SchedulerOutput> {
        SchedulerMachine.output(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Machine, run_machine};
    use crate::machines::scheduler::effect::SchedulerEffect;
    use crate::machines::scheduler::event::{NodeOutcome, SchedulerEvent};
    use crate::machines::scheduler::machine::{SchedulerMachine, SchedulerOutput};
    use crate::machines::scheduler::state::{
        ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest,
        SchedulerState,
    };
    use crate::node_runner::StaticNodeRunner;

    fn handler() -> SchedulerHandler<StaticNodeRunner> {
        SchedulerHandler::new(StaticNodeRunner)
    }

    fn work_node(id: &str, objective: &str) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            objective: objective.to_string(),
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
        }
    }

    #[test]
    fn run_node_effect_uses_node_runner() {
        let h = handler();
        let effect = SchedulerEffect::RunNode {
            node_id: NodeId("n1".to_string()),
            kind: NodeKind::Work,
            objective: "write some code".to_string(),
            model_tier: ModelTier::Cheap,
            attempt: 0,
        };
        let event = h.handle_effect(effect);
        let SchedulerEvent::NodeReturned { outcome, .. } = event else {
            panic!("expected NodeReturned, got {event:#?}");
        };
        assert!(matches!(outcome, NodeOutcome::WorkAccepted(_)));
    }

    #[test]
    fn plan_node_flows_through_runner() {
        let state = SchedulerMachine::initial_state(RunRequest {
            objective: "plan the work".to_string(),
        });
        let output = run_machine(handler(), state);
        assert!(
            matches!(output, SchedulerOutput::Complete { .. }),
            "expected Complete, got {output:#?}"
        );
    }

    #[test]
    fn work_node_flows_through_runner() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("W", "build artifacts")],
                next_id: 0,
            },
        };
        let output = run_machine(handler(), state);
        assert!(
            matches!(output, SchedulerOutput::Complete { .. }),
            "expected Complete, got {output:#?}"
        );
    }

    #[test]
    fn failed_node_flows_through_runner() {
        let state = SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work_node("F", "fail this step")],
                next_id: 0,
            },
        };
        let output = run_machine(handler(), state);
        assert!(
            matches!(output, SchedulerOutput::Failed { .. }),
            "expected Failed, got {output:#?}"
        );
    }
}
