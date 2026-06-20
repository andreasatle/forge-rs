use std::collections::HashSet;

use crate::engine::{Machine, Transition};

use super::effect::SchedulerEffect;
use super::event::{
    NodeFailure, NodeOutcome, NodeOutcome::*, NodeRequest, RecoveryAction, SchedulerEvent,
    WorkOutput,
};
use super::state::{ModelTier, Node, NodeId, NodeKind, NodeStatus, RunGraph, SchedulerState};

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

    /// Complete when no node is still Pending or Running.
    /// Failed/Cancelled nodes are dead; Terminal failures exit immediately via SchedulerState::Failed.
    /// TODO: track a separate "active leaf count" when cancellation propagation is added.
    fn all_complete(graph: &RunGraph) -> bool {
        !graph
            .nodes
            .iter()
            .any(|n| matches!(n.status, NodeStatus::Pending | NodeStatus::Running))
    }

    fn mark_node(graph: RunGraph, node_id: &NodeId, status: NodeStatus) -> RunGraph {
        let next_id = graph.next_id;
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
            next_id,
        }
    }

    fn mark_node_completed_with_summary(
        graph: RunGraph,
        node_id: &NodeId,
        summary: String,
    ) -> RunGraph {
        let next_id = graph.next_id;
        RunGraph {
            nodes: graph
                .nodes
                .into_iter()
                .map(|mut n| {
                    if &n.id == node_id {
                        n.status = NodeStatus::Completed;
                        n.summary = Some(summary.clone());
                    }
                    n
                })
                .collect(),
            next_id,
        }
    }

    fn get_node<'a>(graph: &'a RunGraph, node_id: &NodeId) -> &'a Node {
        graph
            .nodes
            .iter()
            .find(|n| &n.id == node_id)
            .expect("node not found in graph")
    }

    /// Push a new node and advance the ID counter.
    fn push_node(mut graph: RunGraph, node: Node) -> RunGraph {
        graph.nodes.push(node);
        graph.next_id += 1;
        graph
    }

    fn insert_children(
        mut graph: RunGraph,
        parent_id: &NodeId,
        children: Vec<NodeRequest>,
    ) -> RunGraph {
        for req in children {
            let id = NodeId(format!("{}-child-{}", parent_id.0, graph.next_id));
            graph.next_id += 1;
            graph.nodes.push(Node {
                id,
                kind: req.kind,
                objective: req.objective,
                dependencies: req.dependencies,
                status: NodeStatus::Pending,
                attempt: 0,
                model_tier: ModelTier::Cheap,
                summary: None,
            });
        }
        graph
    }

    fn apply_retry(graph: RunGraph, node_id: &NodeId) -> RunGraph {
        let (kind, objective, deps, attempt, model_tier) = {
            let n = Self::get_node(&graph, node_id);
            (
                n.kind.clone(),
                n.objective.clone(),
                n.dependencies.clone(),
                n.attempt,
                n.model_tier.clone(),
            )
        };
        let replacement_id = NodeId(format!("{}-retry-{}", node_id.0, graph.next_id));
        let replacement = Node {
            id: replacement_id,
            kind,
            objective,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            model_tier,
            summary: None,
        };
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        Self::push_node(graph, replacement)
        // TODO: dependency rewiring — downstream nodes that depend on node_id will stall
        // because node_id is Failed, not Completed. Resolution requires remapping or
        // making completion-check dependency-aware of retry chains.
    }

    fn apply_split(graph: RunGraph, node_id: &NodeId, message: String) -> RunGraph {
        let (deps, model_tier) = {
            let n = Self::get_node(&graph, node_id);
            (n.dependencies.clone(), n.model_tier.clone())
        };
        let _ = model_tier; // Split always uses Strong to maximize plan quality
        let split_id = NodeId(format!("{}-split-{}", node_id.0, graph.next_id));
        let split_node = Node {
            id: split_id,
            kind: NodeKind::Plan,
            objective: message,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: 0,
            model_tier: ModelTier::Strong,
            summary: None,
        };
        // Mark original Failed (not Cancelled) so the audit trail is unambiguous.
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        Self::push_node(graph, split_node)
    }

    fn apply_elevate(graph: RunGraph, node_id: &NodeId) -> RunGraph {
        let (kind, objective, deps, attempt) = {
            let n = Self::get_node(&graph, node_id);
            (
                n.kind.clone(),
                n.objective.clone(),
                n.dependencies.clone(),
                n.attempt,
            )
        };
        let elevated_id = NodeId(format!("{}-elevated-{}", node_id.0, graph.next_id));
        let replacement = Node {
            id: elevated_id,
            kind,
            objective,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            model_tier: ModelTier::Strong,
            summary: None,
        };
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        Self::push_node(graph, replacement)
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
                        state: SchedulerState::Complete {
                            graph: graph.clone(),
                        },
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
                let (kind, objective, model_tier, attempt) = {
                    let n = Self::get_node(&graph, &node_id);
                    (
                        n.kind.clone(),
                        n.objective.clone(),
                        n.model_tier.clone(),
                        n.attempt,
                    )
                };
                let effect = SchedulerEffect::RunNode {
                    node_id: node_id.clone(),
                    kind,
                    objective,
                    model_tier,
                    attempt,
                };
                let graph = Self::mark_node(graph, &node_id, NodeStatus::Running);
                Transition {
                    state: SchedulerState::Waiting {
                        graph,
                        running: node_id,
                    },
                    effects: vec![effect],
                }
            }

            (
                SchedulerState::Waiting { graph, running },
                SchedulerEvent::NodeReturned { node_id, outcome },
            ) => {
                assert_eq!(
                    running, node_id,
                    "returned node does not match running node"
                );

                match outcome {
                    PlanAccepted(plan) => {
                        let graph = Self::mark_node(graph, &node_id, NodeStatus::Completed);
                        let graph = Self::insert_children(graph, &node_id, plan.children);
                        Transition {
                            state: SchedulerState::SelectingReady { graph },
                            effects: vec![],
                        }
                    }

                    WorkAccepted(work) => {
                        let graph =
                            Self::mark_node_completed_with_summary(graph, &node_id, work.summary);
                        Transition {
                            state: SchedulerState::SelectingReady { graph },
                            effects: vec![],
                        }
                    }

                    Failed(NodeFailure {
                        reason: _,
                        recovery,
                    }) => match recovery {
                        RecoveryAction::Retry { .. } => {
                            let graph = Self::apply_retry(graph, &node_id);
                            Transition {
                                state: SchedulerState::SelectingReady { graph },
                                effects: vec![],
                            }
                        }

                        RecoveryAction::Split { message } => {
                            let graph = Self::apply_split(graph, &node_id, message);
                            Transition {
                                state: SchedulerState::SelectingReady { graph },
                                effects: vec![],
                            }
                        }

                        RecoveryAction::ElevateModel { .. } => {
                            let graph = Self::apply_elevate(graph, &node_id);
                            Transition {
                                state: SchedulerState::SelectingReady { graph },
                                effects: vec![],
                            }
                        }

                        RecoveryAction::Terminal { message } => {
                            let graph = Self::mark_node(graph, &node_id, NodeStatus::Failed);
                            Transition {
                                state: SchedulerState::Failed {
                                    graph: graph.clone(),
                                    reason: message.clone(),
                                },
                                effects: vec![SchedulerEffect::ReturnFailed {
                                    graph,
                                    reason: message,
                                }],
                            }
                        }
                    },
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
            SchedulerEffect::RunNode {
                node_id,
                kind: _,
                objective,
                model_tier: _,
                attempt,
            } => {
                println!(
                    "  -> running node {} (attempt {}): {:?}",
                    node_id.0, attempt, objective
                );

                let outcome = if objective.contains("plan") {
                    NodeOutcome::PlanAccepted(super::event::PlanOutput {
                        children: vec![NodeRequest {
                            kind: NodeKind::Work,
                            objective: format!("work from {}", node_id.0),
                            dependencies: vec![node_id.clone()],
                        }],
                    })
                } else if objective.contains("retry") {
                    if attempt == 0 {
                        NodeOutcome::Failed(NodeFailure {
                            reason: "first attempt failed".to_string(),
                            recovery: RecoveryAction::Retry {
                                message: "try again".to_string(),
                            },
                        })
                    } else {
                        NodeOutcome::WorkAccepted(WorkOutput {
                            summary: format!("retry succeeded on attempt {attempt}"),
                        })
                    }
                } else if objective.contains("split") {
                    NodeOutcome::Failed(NodeFailure {
                        reason: "task too complex to execute directly".to_string(),
                        recovery: RecoveryAction::Split {
                            message: format!("decompose: {objective}"),
                        },
                    })
                } else if objective.contains("elevate") {
                    if attempt == 0 {
                        NodeOutcome::Failed(NodeFailure {
                            reason: "needs stronger model".to_string(),
                            recovery: RecoveryAction::ElevateModel {
                                message: "retry with strong model".to_string(),
                            },
                        })
                    } else {
                        NodeOutcome::WorkAccepted(WorkOutput {
                            summary: format!("elevated model succeeded on attempt {attempt}"),
                        })
                    }
                } else if objective.contains("terminal") {
                    NodeOutcome::Failed(NodeFailure {
                        reason: "unrecoverable error".to_string(),
                        recovery: RecoveryAction::Terminal {
                            message: "fatal: cannot continue".to_string(),
                        },
                    })
                } else {
                    NodeOutcome::WorkAccepted(WorkOutput {
                        summary: format!("completed: {objective}"),
                    })
                };

                SchedulerEvent::NodeReturned { node_id, outcome }
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
    use crate::machines::scheduler::event::{
        NodeFailure, NodeOutcome, NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
    };
    use crate::machines::scheduler::state::{Node, RunGraph};

    fn work_node(id: &str, objective: &str, deps: &[&str]) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            objective: objective.to_string(),
            dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
            status: NodeStatus::Pending,
            attempt: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
        }
    }

    fn plan_node(id: &str, objective: &str, deps: &[&str]) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Plan,
            objective: objective.to_string(),
            dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
            status: NodeStatus::Pending,
            attempt: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
        }
    }

    fn single_work_graph() -> RunGraph {
        RunGraph {
            nodes: vec![work_node("A", "do a thing", &[])],
            next_id: 0,
        }
    }

    fn chain_graph() -> RunGraph {
        RunGraph {
            nodes: vec![
                work_node("A", "step one", &[]),
                work_node("B", "step two", &["A"]),
                work_node("C", "step three", &["B"]),
            ],
            next_id: 0,
        }
    }

    fn running(mut graph: RunGraph, id: &str) -> RunGraph {
        for n in &mut graph.nodes {
            if n.id.0 == id {
                n.status = NodeStatus::Running;
            }
        }
        graph
    }

    fn do_transition(
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        SchedulerMachine.transition(state, event)
    }

    // ── existing structural tests ──────────────────────────────────────────────

    #[test]
    fn not_started_start_moves_to_selecting_ready() {
        let t = do_transition(
            SchedulerState::NotStarted {
                graph: single_work_graph(),
            },
            SchedulerEvent::Start,
        );
        assert!(matches!(t.state, SchedulerState::SelectingReady { .. }));
        assert!(t.effects.is_empty());
    }

    #[test]
    fn selecting_ready_with_ready_node_moves_to_dispatching() {
        let t = do_transition(
            SchedulerState::SelectingReady {
                graph: single_work_graph(),
            },
            SchedulerEvent::Start,
        );
        assert!(matches!(t.state, SchedulerState::Dispatching { .. }));
        assert!(t.effects.is_empty());
    }

    #[test]
    fn selecting_ready_all_complete_moves_to_complete() {
        let mut graph = single_work_graph();
        graph.nodes[0].status = NodeStatus::Completed;
        let t = do_transition(
            SchedulerState::SelectingReady { graph },
            SchedulerEvent::Start,
        );
        assert!(matches!(t.state, SchedulerState::Complete { .. }));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnComplete { .. }]
        ));
    }

    #[test]
    fn selecting_ready_no_ready_moves_to_failed() {
        let graph = RunGraph {
            nodes: vec![work_node("B", "blocked", &["A"])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::SelectingReady { graph },
            SchedulerEvent::Start,
        );
        assert!(matches!(t.state, SchedulerState::Failed { .. }));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn dispatching_start_emits_run_node_and_marks_running() {
        let graph = single_work_graph();
        let ready = vec![NodeId("A".to_string())];
        let t = do_transition(
            SchedulerState::Dispatching { graph, ready },
            SchedulerEvent::Start,
        );

        let SchedulerState::Waiting { graph, running } = t.state else {
            panic!("expected Waiting")
        };
        assert_eq!(running.0, "A");
        assert_eq!(graph.nodes[0].status, NodeStatus::Running);
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::RunNode { .. }]
        ));
    }

    // ── new outcome tests ──────────────────────────────────────────────────────

    #[test]
    fn plan_node_creates_work_child() {
        let graph = RunGraph {
            nodes: vec![plan_node("P", "plan something", &[])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "P"),
                running: NodeId("P".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("P".to_string()),
                outcome: NodeOutcome::PlanAccepted(PlanOutput {
                    children: vec![NodeRequest {
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
                        dependencies: vec![NodeId("P".to_string())],
                    }],
                }),
            },
        );

        let SchedulerState::SelectingReady { graph } = t.state else {
            panic!("expected SelectingReady")
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].kind, NodeKind::Work);
        assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
        assert_eq!(graph.nodes[1].dependencies, vec![NodeId("P".to_string())]);
    }

    #[test]
    fn work_node_accepted_marks_completed_with_summary() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "A"),
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("A".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "done!".to_string(),
                }),
            },
        );

        let SchedulerState::SelectingReady { graph } = t.state else {
            panic!("expected SelectingReady")
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert_eq!(graph.nodes[0].summary, Some("done!".to_string()));
    }

    #[test]
    fn retry_creates_replacement_node() {
        let graph = RunGraph {
            nodes: vec![work_node("W", "do retry", &[])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    reason: "first try failed".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "try again".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::SelectingReady { graph } = t.state else {
            panic!("expected SelectingReady")
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert_eq!(graph.nodes.len(), 2);
        let replacement = &graph.nodes[1];
        assert_eq!(replacement.status, NodeStatus::Pending);
        assert_eq!(replacement.attempt, 1);
        assert_eq!(replacement.model_tier, ModelTier::Cheap);
        assert_eq!(replacement.objective, "do retry");
    }

    #[test]
    fn elevate_creates_replacement_node_with_strong_tier() {
        let graph = RunGraph {
            nodes: vec![work_node("W", "do elevate", &[])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    reason: "needs stronger model".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use strong".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::SelectingReady { graph } = t.state else {
            panic!("expected SelectingReady")
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert_eq!(graph.nodes.len(), 2);
        let replacement = &graph.nodes[1];
        assert_eq!(replacement.status, NodeStatus::Pending);
        assert_eq!(replacement.attempt, 1);
        assert_eq!(replacement.model_tier, ModelTier::Strong);
        assert_eq!(replacement.objective, "do elevate");
    }

    #[test]
    fn terminal_failure_produces_failed_scheduler_output() {
        let graph = RunGraph {
            nodes: vec![Node {
                id: NodeId("T".to_string()),
                kind: NodeKind::Work,
                objective: "terminal task".to_string(),
                dependencies: vec![],
                status: NodeStatus::Pending,
                attempt: 0,
                model_tier: ModelTier::Cheap,
                summary: None,
            }],
            next_id: 0,
        };
        let output =
            crate::engine::run_machine(SchedulerMachine, SchedulerState::NotStarted { graph });
        assert!(matches!(output, SchedulerOutput::Failed { .. }));
    }

    #[test]
    fn dependencies_block_pending_nodes() {
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "first", &[]),
                work_node("B", "second", &["A"]),
            ],
            next_id: 0,
        };

        let ready = SchedulerMachine::find_ready(&graph);
        assert_eq!(ready, vec![NodeId("A".to_string())]);

        let mut graph2 = graph.clone();
        graph2.nodes[0].status = NodeStatus::Completed;
        let ready2 = SchedulerMachine::find_ready(&graph2);
        assert_eq!(ready2, vec![NodeId("B".to_string())]);
    }

    #[test]
    fn full_chain_run() {
        let output = crate::engine::run_machine(
            SchedulerMachine,
            SchedulerState::NotStarted {
                graph: chain_graph(),
            },
        );
        let SchedulerOutput::Complete(graph) = output else {
            panic!("expected Complete")
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.status == NodeStatus::Completed)
        );
    }
}
