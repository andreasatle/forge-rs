use std::collections::HashSet;

use crate::runner::Machine;
use crate::transition::Transition;

use super::effect::SchedulerEffect;
use super::event::SchedulerEvent;
use super::state::{NodeId, NodeStatus, RunGraph, SchedulerState};

#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerOutput {
    Complete(RunGraph),
    Failed { graph: RunGraph, reason: String },
}

pub struct SchedulerMachine;

impl SchedulerMachine {
    fn find_ready(graph: &RunGraph) -> Vec<NodeId> {
        let completed: HashSet<&NodeId> = graph
            .nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .map(|n| &n.id)
            .collect();

        graph
            .nodes
            .iter()
            .filter(|n| {
                n.status == NodeStatus::Pending
                    && n.dependencies.iter().all(|dep| completed.contains(dep))
            })
            .map(|n| n.id.clone())
            .collect()
    }

    fn all_complete(graph: &RunGraph) -> bool {
        graph.nodes.iter().all(|n| n.status == NodeStatus::Completed)
    }

    fn mark_node(graph: RunGraph, node_id: &NodeId, status: NodeStatus) -> RunGraph {
        RunGraph {
            nodes: graph
                .nodes
                .into_iter()
                .map(|mut n| {
                    if &n.id == node_id {
                        n.status = status.clone();
                    }
                    n
                })
                .collect(),
        }
    }
}

impl Machine for SchedulerMachine {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerOutput;

    fn start_event(&self) -> Self::Event {
        SchedulerEvent::Start
    }

    fn transition(
        &self,
        state: Self::State,
        event: Self::Event,
    ) -> Transition<Self::State, Self::Effect> {
        println!("STATE: {state:#?}");
        println!("EVENT: {event:#?}");

        match (state, event) {
            (SchedulerState::NotStarted { graph }, SchedulerEvent::Start) => Transition {
                state: SchedulerState::SelectingReady { graph },
                effects: vec![],
            },

            (SchedulerState::SelectingReady { graph }, SchedulerEvent::Start) => {
                if Self::all_complete(&graph) {
                    Transition {
                        state: SchedulerState::Complete { graph: graph.clone() },
                        effects: vec![SchedulerEffect::ReturnComplete { graph }],
                    }
                } else {
                    let ready = Self::find_ready(&graph);
                    if ready.is_empty() {
                        let reason = "no ready nodes and graph is not complete".to_string();
                        Transition {
                            state: SchedulerState::Failed {
                                graph: graph.clone(),
                                reason: reason.clone(),
                            },
                            effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                        }
                    } else {
                        Transition {
                            state: SchedulerState::Dispatching { graph, ready },
                            effects: vec![],
                        }
                    }
                }
            }

            (SchedulerState::Dispatching { graph, ready }, SchedulerEvent::Start) => {
                let node_id = ready[0].clone();
                let graph = Self::mark_node(graph, &node_id, NodeStatus::Running);
                Transition {
                    state: SchedulerState::Waiting {
                        graph,
                        running: node_id.clone(),
                    },
                    effects: vec![SchedulerEffect::RunNode { node_id }],
                }
            }

            (
                SchedulerState::Waiting { graph, running },
                SchedulerEvent::NodeCompleted { node_id },
            ) => {
                assert_eq!(running, node_id, "completed node does not match running node");
                let graph = Self::mark_node(graph, &node_id, NodeStatus::Completed);
                Transition {
                    state: SchedulerState::SelectingReady { graph },
                    effects: vec![],
                }
            }

            (
                SchedulerState::Waiting { graph, running },
                SchedulerEvent::NodeFailed { node_id, reason },
            ) => {
                assert_eq!(running, node_id, "failed node does not match running node");
                let graph = Self::mark_node(graph, &node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph: graph.clone(),
                        reason: reason.clone(),
                    },
                    effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                }
            }

            (state, event) => {
                panic!("invalid transition: state={state:#?}, event={event:#?}");
            }
        }
    }

    fn handle_effect(&self, effect: Self::Effect) -> Self::Event {
        println!("EFFECT: {effect:#?}");

        match effect {
            SchedulerEffect::RunNode { node_id } => {
                println!("  -> running node {}", node_id.0);
                SchedulerEvent::NodeCompleted { node_id }
            }

            SchedulerEffect::ReturnComplete { .. } | SchedulerEffect::ReturnFailed { .. } => {
                unreachable!("return effects are never dispatched to the effect handler")
            }
        }
    }

    fn output(&self, state: &Self::State) -> Option<Self::Output> {
        match state {
            SchedulerState::Complete { graph } => Some(SchedulerOutput::Complete(graph.clone())),
            SchedulerState::Failed { graph, reason } => Some(SchedulerOutput::Failed {
                graph: graph.clone(),
                reason: reason.clone(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::scheduler::state::{Node, RunGraph};

    fn node(id: &str, deps: &[&str]) -> Node {
        Node {
            id: NodeId(id.to_string()),
            dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
            status: NodeStatus::Pending,
        }
    }

    fn single_node_graph() -> RunGraph {
        RunGraph { nodes: vec![node("A", &[])] }
    }

    fn chain_graph() -> RunGraph {
        RunGraph {
            nodes: vec![node("A", &[]), node("B", &["A"]), node("C", &["B"])],
        }
    }

    fn transition(
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        SchedulerMachine.transition(state, event)
    }

    #[test]
    fn not_started_start_moves_to_selecting_ready() {
        let t = transition(
            SchedulerState::NotStarted { graph: single_node_graph() },
            SchedulerEvent::Start,
        );
        assert!(matches!(t.state, SchedulerState::SelectingReady { .. }));
        assert!(t.effects.is_empty());
    }

    #[test]
    fn selecting_ready_with_ready_node_moves_to_dispatching() {
        let t = transition(
            SchedulerState::SelectingReady { graph: single_node_graph() },
            SchedulerEvent::Start,
        );
        assert!(matches!(t.state, SchedulerState::Dispatching { .. }));
        assert!(t.effects.is_empty());
    }

    #[test]
    fn selecting_ready_all_complete_moves_to_complete() {
        let mut graph = single_node_graph();
        graph.nodes[0].status = NodeStatus::Completed;

        let t = transition(SchedulerState::SelectingReady { graph }, SchedulerEvent::Start);
        assert!(matches!(t.state, SchedulerState::Complete { .. }));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnComplete { .. }]
        ));
    }

    #[test]
    fn selecting_ready_no_ready_moves_to_failed() {
        let mut graph = RunGraph {
            nodes: vec![node("B", &["A"])],
        };
        graph.nodes[0].status = NodeStatus::Pending;

        let t = transition(SchedulerState::SelectingReady { graph }, SchedulerEvent::Start);
        assert!(matches!(t.state, SchedulerState::Failed { .. }));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn dispatching_start_emits_run_node_and_marks_running() {
        let graph = single_node_graph();
        let ready = vec![NodeId("A".to_string())];

        let t = transition(
            SchedulerState::Dispatching { graph, ready },
            SchedulerEvent::Start,
        );

        let SchedulerState::Waiting { graph, running } = t.state else {
            panic!("expected Waiting");
        };
        assert_eq!(running.0, "A");
        assert_eq!(graph.nodes[0].status, NodeStatus::Running);
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::RunNode { .. }]
        ));
    }

    #[test]
    fn waiting_node_completed_marks_complete_and_selects_ready() {
        let mut graph = single_node_graph();
        graph.nodes[0].status = NodeStatus::Running;

        let t = transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeCompleted { node_id: NodeId("A".to_string()) },
        );

        let SchedulerState::SelectingReady { graph } = t.state else {
            panic!("expected SelectingReady");
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert!(t.effects.is_empty());
    }

    #[test]
    fn waiting_node_failed_moves_to_failed() {
        let mut graph = single_node_graph();
        graph.nodes[0].status = NodeStatus::Running;

        let t = transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeFailed {
                node_id: NodeId("A".to_string()),
                reason: "boom".to_string(),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed");
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert_eq!(reason, "boom");
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn full_chain_run() {
        use crate::runner::run_machine;

        let graph = chain_graph();
        let output = run_machine(SchedulerMachine, SchedulerState::NotStarted { graph });

        let SchedulerOutput::Complete(graph) = output else {
            panic!("expected Complete");
        };
        assert!(graph.nodes.iter().all(|n| n.status == NodeStatus::Completed));
        assert_eq!(graph.nodes[0].id.0, "A");
        assert_eq!(graph.nodes[1].id.0, "B");
        assert_eq!(graph.nodes[2].id.0, "C");
    }
}
