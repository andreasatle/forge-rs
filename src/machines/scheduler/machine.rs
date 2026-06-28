//! Scheduler machine — state-machine entry point.
//!
//! Owns `SchedulerMachine`, `SchedulerOutput`, `RecoverySummary`, and the
//! `transition` and `output` functions. Pure graph helpers live in `graph.rs`;
//! recovery routing and application live in `recovery.rs`.

use crate::engine::Transition;

use super::effect::SchedulerEffect;
use super::event::{
    IntegrationFailure, IntegrationOutcome, NodeFailure, NodeOutcome::*, SchedulerEvent,
};
use super::state::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest, SchedulerState,
};
use super::{graph, recovery};

// Re-expose constants so the nested test module sees them via `use super::*`.
#[cfg(test)]
use super::graph::{MAX_ATTEMPTS, MAX_GRAPH_NODES, MAX_PLAN_DEPTH};

/// A typed summary of what recovery actions occurred during a successful run.
///
/// Derived from the final `RunGraph` by inspecting each node's `NodeOrigin`.
/// A clean run where no recovery was needed has all counts at zero and
/// `recovered == false`.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoverySummary {
    /// `true` if any recovery action (Retry, ElevateModel, or Split) occurred.
    pub recovered: bool,
    /// Number of nodes created by `Retry` recovery.
    pub retry_count: usize,
    /// Number of nodes created by `ElevateModel` recovery.
    pub elevate_count: usize,
    /// Number of nodes created by `Split` recovery.
    pub split_count: usize,
}

impl RecoverySummary {
    fn from_graph(graph: &RunGraph) -> Self {
        let mut retry_count = 0usize;
        let mut elevate_count = 0usize;
        let mut split_count = 0usize;
        for node in &graph.nodes {
            match &node.origin {
                NodeOrigin::Retry { .. } => retry_count += 1,
                NodeOrigin::ElevateModel { .. } => elevate_count += 1,
                NodeOrigin::Split { .. } => split_count += 1,
                NodeOrigin::Root | NodeOrigin::PlanExpansion => {}
            }
        }
        let recovered = retry_count + elevate_count + split_count > 0;
        RecoverySummary {
            recovered,
            retry_count,
            elevate_count,
            split_count,
        }
    }
}

/// The terminal result of a complete scheduler run.
///
/// The caller (`run_machine` or `RunMachine`) receives this when the scheduler
/// reaches either of its two terminal states.
///
/// `Complete` distinguishes a clean run (no recovery) from a run that reached
/// completion only after one or more recovery actions (Retry, ElevateModel, or
/// Split). Inspect `recovery_summary` to determine which path was taken without
/// re-scanning the graph.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerOutput {
    /// Every node reached a terminal status and the run succeeded.
    Complete {
        /// The final graph with every node in a terminal status.
        graph: RunGraph,
        /// A typed account of which recovery actions occurred during the run.
        /// All counts are zero and `recovered` is `false` for a clean run.
        recovery_summary: RecoverySummary,
    },
    /// A `Terminal` recovery was triggered, halting the run. The graph is
    /// returned in its current state so the caller can inspect what succeeded
    /// before the failure.
    Failed {
        /// The graph at the point of failure, for post-mortem inspection.
        graph: RunGraph,
        /// A human-readable explanation of why the run was halted.
        reason: String,
    },
}

/// The scheduler state machine.
///
/// All durable data travels inside `SchedulerState`. This type carries only
/// static policy that is fixed for the lifetime of a run.
pub struct SchedulerMachine {
    /// Whether a distinct strong-tier model is configured.
    ///
    /// When `false`, `ElevateModel` recovery cannot produce a meaningfully
    /// different result and is demoted to a `Retry` instead (or `Terminal` when
    /// attempts are exhausted).
    pub has_strong_tier: bool,
}

impl SchedulerMachine {
    /// Build the initial scheduler state from an external run request.
    ///
    /// Creates a `SchedulerState::Running` containing a single root `Plan` node
    /// whose objective is taken from the request. All other node fields are set
    /// to their default starting values.
    pub fn initial_state(request: RunRequest) -> SchedulerState {
        let root = Node {
            id: NodeId("root".to_string()),
            kind: NodeKind::Plan,
            objective: request.objective,
            target_files: vec![],
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
        };
        SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![root],
                next_id: 0,
            },
        }
    }

    // All graph helpers live in graph.rs and recovery.rs.
}

#[cfg(test)]
impl SchedulerMachine {
    pub(super) fn find_ready(g: &RunGraph) -> Vec<NodeId> {
        graph::find_ready(g)
    }
}

impl SchedulerMachine {
    /// Returns the event used to bootstrap the scheduler on the first tick.
    pub fn start_event(&self) -> SchedulerEvent {
        SchedulerEvent::Start
    }

    /// Pure transition function: given the current state and an event, returns
    /// the next state and any effects to dispatch.
    pub fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        match (state, event) {
            // Scan the graph, then in the same tick either complete, fail, or dispatch.
            //
            // Three outcomes:
            //   1. All nodes are terminal → emit ReturnComplete and stop.
            //   2. Some nodes are Pending but none are ready → deadlock; emit ReturnFailed.
            //   3. At least one node is ready → mark it Running, emit RunNode, move to Waiting.
            (SchedulerState::Running { graph }, SchedulerEvent::Start) => {
                if let Err(reason) = graph::validate_graph_invariants(&graph) {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    };
                }
                let active = graph::active_nodes(&graph);
                if let Some(node) = active.first() {
                    let reason = format!(
                        "invalid running state: node {} is {:?}",
                        node.id.0, node.status
                    );
                    return recovery::failed_transition(graph, reason);
                }
                if graph::all_complete(&graph) {
                    Transition {
                        state: SchedulerState::Complete {
                            graph: graph.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnComplete { graph }],
                    }
                } else {
                    let ready = graph::find_ready(&graph);
                    if ready.is_empty() {
                        let reason = graph::diagnose_no_ready(&graph);
                        Transition {
                            state: SchedulerState::Failed {
                                graph: graph.clone(),
                                reason: reason.clone(),
                            },
                            effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                        }
                    } else {
                        let node_id = ready[0].clone();
                        let (kind, objective, target_files, model_tier, attempt) = {
                            let n = graph::get_node(&graph, &node_id);
                            (
                                n.kind.clone(),
                                n.objective.clone(),
                                n.target_files.clone(),
                                n.model_tier,
                                n.attempt,
                            )
                        };
                        let effect = SchedulerEffect::RunNode {
                            node_id: node_id.clone(),
                            kind,
                            objective,
                            target_files,
                            model_tier,
                            attempt,
                        };
                        let graph = graph::mark_node(graph, &node_id, NodeStatus::Running);
                        Transition {
                            state: SchedulerState::Waiting {
                                graph,
                                running: node_id,
                            },
                            effects: vec![effect],
                        }
                    }
                }
            }

            // Node returned: react to what the node produced.
            //
            // The assertion guards against a race condition that cannot happen in the
            // single-threaded runner but would be catastrophic if it did: a result for
            // a node that was never dispatched.
            (
                SchedulerState::Waiting { graph, running },
                SchedulerEvent::NodeReturned { node_id, outcome },
            ) => {
                if let Err(reason) = graph::validate_waiting_state(&graph, &running) {
                    return recovery::failed_transition(graph, reason);
                }

                if running != node_id {
                    let reason = format!(
                        "protocol violation: expected result for node {} but received {}",
                        running.0, node_id.0
                    );
                    return recovery::failed_transition(graph, reason);
                }

                // Validate that the node is in Running status (not Integrating or other).
                if let Some(reason) = graph::invalid_node_return_reason(&graph, &node_id) {
                    return recovery::failed_transition(graph, reason);
                }

                // Validate that the outcome is compatible with the node's kind.
                let node_kind = graph::get_node(&graph, &node_id).kind.clone();
                if let Some(reason) =
                    graph::invalid_node_outcome_reason(&node_id, &node_kind, &outcome)
                {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    };
                }

                match outcome {
                    // A successful planner expands the graph: the plan node is marked
                    // Completed and its requested children are inserted as new Pending
                    // nodes. The scheduler then re-scans for ready nodes.
                    //
                    // Validation runs first, before any mutation, so an invalid plan
                    // does not insert children. A plan-depth violation additionally
                    // marks the original plan Failed as the circuit breaker source.
                    PlanAccepted(plan) => {
                        let parent_depth = graph::get_node(&graph, &node_id).plan_depth;
                        match graph::validate_plan_dependencies(&graph, &plan.children) {
                            Err(reason) => Transition {
                                state: SchedulerState::Failed {
                                    graph: graph.clone(),
                                    reason: reason.clone(),
                                },
                                effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                            },
                            Ok(()) if !graph::graph_has_capacity(&graph, plan.children.len()) => {
                                let reason = graph::graph_size_limit_reason(plan.children.len());
                                Transition {
                                    state: SchedulerState::Failed {
                                        graph: graph.clone(),
                                        reason: reason.clone(),
                                    },
                                    effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                                }
                            }
                            Ok(()) => {
                                if let Err(reason) =
                                    graph::validate_plan_child_depths(parent_depth, &plan.children)
                                {
                                    let graph =
                                        graph::mark_node(graph, &node_id, NodeStatus::Failed);
                                    return recovery::failed_transition(graph, reason);
                                }
                                let graph =
                                    graph::mark_node(graph, &node_id, NodeStatus::Completed);
                                let graph = graph::insert_children(graph, &node_id, plan.children);
                                Transition {
                                    state: SchedulerState::Running { graph },
                                    effects: vec![],
                                }
                            }
                        }
                    }

                    // Work accepted: the node moves to Integrating and an IntegrateWork
                    // effect is emitted. The node is not yet dependency-satisfying; that
                    // only happens when IntegrationReturned(Succeeded) arrives.
                    WorkAccepted(work) => {
                        let graph = graph::mark_node(graph, &node_id, NodeStatus::Integrating);
                        Transition {
                            state: SchedulerState::Waiting {
                                graph,
                                running: node_id.clone(),
                            },
                            effects: vec![SchedulerEffect::IntegrateWork { node_id, work }],
                        }
                    }

                    Failed(NodeFailure {
                        kind,
                        message,
                        recovery,
                    }) => recovery::route_recovery(
                        self.has_strong_tier,
                        graph,
                        &node_id,
                        kind,
                        message,
                        recovery,
                    ),
                }
            }

            // Integration finished: success marks the node Completed and
            // resumes scanning; failure routes through the same recovery
            // machinery as execution failure.
            (
                SchedulerState::Waiting { graph, running },
                SchedulerEvent::IntegrationReturned { node_id, outcome },
            ) => {
                if let Err(reason) = graph::validate_waiting_state(&graph, &running) {
                    return recovery::failed_transition(graph, reason);
                }

                if running != node_id {
                    let reason = format!(
                        "protocol violation: expected integration result for node {} but received {}",
                        running.0, node_id.0
                    );
                    return recovery::failed_transition(graph, reason);
                }

                // Validate that integration arrives for a Work node in Integrating status.
                if let Some(reason) = graph::invalid_integration_reason(&graph, &node_id) {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    };
                }

                match outcome {
                    IntegrationOutcome::Succeeded(integration_output) => {
                        let graph = graph::mark_node_completed_with_summary(
                            graph,
                            &node_id,
                            integration_output.summary,
                        );
                        Transition {
                            state: SchedulerState::Running { graph },
                            effects: vec![],
                        }
                    }
                    IntegrationOutcome::Failed(IntegrationFailure {
                        kind,
                        message,
                        recovery,
                    }) => recovery::route_recovery(
                        self.has_strong_tier,
                        graph,
                        &node_id,
                        kind,
                        message,
                        recovery,
                    ),
                }
            }

            (SchedulerState::Running { graph }, SchedulerEvent::NodeReturned { .. }) => {
                recovery::failed_transition(
                    graph,
                    "protocol violation: state Running cannot consume NodeReturned".to_string(),
                )
            }

            (SchedulerState::Running { graph }, SchedulerEvent::IntegrationReturned { .. }) => {
                recovery::failed_transition(
                    graph,
                    "protocol violation: state Running cannot consume IntegrationReturned"
                        .to_string(),
                )
            }

            (SchedulerState::Waiting { graph, .. }, SchedulerEvent::Start) => {
                recovery::failed_transition(
                    graph,
                    "protocol violation: state Waiting cannot consume Start".to_string(),
                )
            }

            (state, event) => {
                panic!("invalid transition: state={state:#?}, event={event:#?}");
            }
        }
    }

    /// Recognise terminal states and extract the final output.
    ///
    /// Returns `Some` only for `Complete` and `Failed`, the two states from
    /// which the scheduler cannot advance further. All other states return
    /// `None` to keep the runner loop going.
    pub fn output(&self, state: &SchedulerState) -> Option<SchedulerOutput> {
        match state {
            SchedulerState::Complete { graph } => Some(SchedulerOutput::Complete {
                recovery_summary: RecoverySummary::from_graph(graph),
                graph: graph.clone(),
            }),
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
    use crate::engine::run_machine;
    use crate::machines::scheduler::FailureKind;
    use crate::machines::scheduler::event::{
        IntegrationFailure, IntegrationOutcome, IntegrationOutput, NodeFailure, NodeOutcome,
        NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
    };
    use crate::machines::scheduler::handler::SchedulerHandler;
    use crate::machines::scheduler::state::{Node, RunGraph, RunRequest};
    use crate::node_runner::StaticNodeRunner;

    fn scheduler_handler() -> SchedulerHandler<StaticNodeRunner> {
        SchedulerHandler::new(StaticNodeRunner)
    }

    fn work_node(id: &str, objective: &str, deps: &[&str]) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Work,
            objective: objective.to_string(),
            target_files: vec![],
            dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
        }
    }

    fn plan_node(id: &str, objective: &str, deps: &[&str]) -> Node {
        Node {
            id: NodeId(id.to_string()),
            kind: NodeKind::Plan,
            objective: objective.to_string(),
            target_files: vec![],
            dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
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

    fn graph_with_filler_nodes(first: Node, total_nodes: usize) -> RunGraph {
        let mut nodes = vec![first];
        for i in 1..total_nodes {
            let mut node = work_node(&format!("filler-{i}"), "already done", &[]);
            node.status = NodeStatus::Completed;
            nodes.push(node);
        }
        RunGraph { nodes, next_id: 0 }
    }

    fn do_transition(
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        SchedulerMachine {
            has_strong_tier: true,
        }
        .transition(state, event)
    }

    // ── RunRequest / initial_state tests ──────────────────────────────────────

    #[test]
    fn initial_state_creates_root_plan_node() {
        let request = RunRequest {
            objective: "plan the project".to_string(),
        };
        let state = SchedulerMachine::initial_state(request);
        let SchedulerState::Running { graph } = state else {
            panic!("expected Running");
        };
        assert_eq!(graph.nodes.len(), 1);
        let root = &graph.nodes[0];
        assert_eq!(root.id, NodeId("root".to_string()));
        assert_eq!(root.kind, NodeKind::Plan);
        assert_eq!(root.status, NodeStatus::Pending);
        assert_eq!(root.objective, "plan the project");
        assert!(root.dependencies.is_empty());
        assert_eq!(root.attempt, 0);
        assert_eq!(root.model_tier, ModelTier::Cheap);
    }

    #[test]
    fn run_request_starts_scheduler_end_to_end() {
        let request = RunRequest {
            objective: "plan demo".to_string(),
        };
        let state = SchedulerMachine::initial_state(request);
        let output = run_machine(scheduler_handler(), state);
        assert!(matches!(output, SchedulerOutput::Complete { .. }));
    }

    // ── Running + Start structural tests ──────────────────────────────────────

    #[test]
    fn running_start_all_complete_moves_to_complete() {
        let mut graph = single_work_graph();
        graph.nodes[0].status = NodeStatus::Completed;
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        assert!(matches!(t.state, SchedulerState::Complete { .. }));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnComplete { .. }]
        ));
    }

    #[test]
    fn running_start_no_ready_moves_to_failed() {
        let graph = RunGraph {
            nodes: vec![work_node("B", "blocked", &["A"])],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        assert!(matches!(t.state, SchedulerState::Failed { .. }));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn running_start_dispatches_ready_node_and_waits() {
        let t = do_transition(
            SchedulerState::Running {
                graph: single_work_graph(),
            },
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
                        id: NodeId("child-1".to_string()),
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
                        target_files: vec![],
                        dependencies: vec![NodeId("P".to_string())],
                    }],
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running")
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].kind, NodeKind::Work);
        assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
        assert_eq!(graph.nodes[1].dependencies, vec![NodeId("P".to_string())]);
    }

    #[test]
    fn plan_child_depth_limit_fails_scheduler() {
        let mut graph = RunGraph {
            nodes: vec![plan_node("P", "plan something", &[])],
            next_id: 0,
        };
        graph.nodes[0].plan_depth = MAX_PLAN_DEPTH;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "P"),
                running: NodeId("P".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("P".to_string()),
                outcome: NodeOutcome::PlanAccepted(PlanOutput {
                    children: vec![NodeRequest {
                        id: NodeId("nested-plan".to_string()),
                        kind: NodeKind::Plan,
                        objective: "nested plan".to_string(),
                        target_files: vec![],
                        dependencies: vec![NodeId("P".to_string())],
                    }],
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 1, "must not insert child plan");
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            reason.contains("plan depth limit") && reason.contains(&MAX_PLAN_DEPTH.to_string()),
            "unexpected reason: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { reason, .. }]
                if reason.contains("plan depth limit")
                    && reason.contains(&MAX_PLAN_DEPTH.to_string())
        ));
    }

    #[test]
    fn work_node_accepted_marks_integrating_and_emits_integrate_work() {
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

        let SchedulerState::Waiting { graph, running } = t.state else {
            panic!("expected Waiting")
        };
        assert_eq!(running, NodeId("A".to_string()));
        assert_eq!(graph.nodes[0].status, NodeStatus::Integrating);
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::IntegrateWork { .. }]
        ));
    }

    #[test]
    fn work_accepted_emits_integration_and_does_not_complete_node() {
        let graph = single_work_graph();
        let node_id = NodeId("A".to_string());

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "A"),
                running: node_id.clone(),
            },
            SchedulerEvent::NodeReturned {
                node_id: node_id.clone(),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "work done".to_string(),
                }),
            },
        );

        let SchedulerState::Waiting {
            ref graph,
            ref running,
        } = t.state
        else {
            panic!("expected Waiting, got {:#?}", t.state);
        };
        assert_eq!(*running, node_id);
        assert_ne!(
            graph.nodes[0].status,
            NodeStatus::Completed,
            "WorkAccepted must not complete the node"
        );
        assert_eq!(graph.nodes[0].status, NodeStatus::Integrating);

        assert_eq!(t.effects.len(), 1, "expected exactly one effect");
        assert!(matches!(
            &t.effects[0],
            SchedulerEffect::IntegrateWork { node_id: id, .. } if *id == node_id
        ));
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
                    kind: FailureKind::DeliberationFailure,
                    message: "first try failed".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "try again".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running")
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
    fn validation_failure_creates_retry_feedback() {
        let mut graph = RunGraph {
            nodes: vec![work_node("W", "fix main", &[])],
            next_id: 0,
        };
        graph.nodes[0].target_files = vec!["main.py".to_string()];
        graph.nodes[0].status = NodeStatus::Integrating;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("W".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::ValidationFailure,
                    message: "validation failed".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "previous validation command: validate main.py\nexit code: 2\nstdout:\nchecking\nstderr:\ninvalid syntax".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        let retry = &graph.nodes[1];
        assert_eq!(retry.status, NodeStatus::Pending);
        assert_eq!(retry.attempt, 1);
        assert_eq!(retry.target_files, vec!["main.py"]);
        assert!(retry.objective.contains("fix main"));
        assert!(retry.objective.contains("Original objective: fix main"));
        assert!(retry.objective.contains("Target files: main.py"));
        assert!(
            retry
                .objective
                .contains("previous validation command: validate main.py")
        );
        assert!(retry.objective.contains("invalid syntax"));
    }

    #[test]
    fn retry_preserves_depth() {
        let mut graph = RunGraph {
            nodes: vec![work_node("W", "do retry", &[])],
            next_id: 0,
        };
        graph.nodes[0].plan_depth = 7;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "first try failed".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "try again".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].plan_depth, 7);
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
                    kind: FailureKind::DeliberationFailure,
                    message: "needs stronger model".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use strong".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running")
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
    fn elevate_preserves_depth() {
        let mut graph = RunGraph {
            nodes: vec![work_node("W", "do elevate", &[])],
            next_id: 0,
        };
        graph.nodes[0].plan_depth = 7;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "needs stronger model".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use strong".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].plan_depth, 7);
    }

    #[test]
    fn terminal_failure_produces_failed_scheduler_output() {
        let graph = RunGraph {
            nodes: vec![Node {
                id: NodeId("T".to_string()),
                kind: NodeKind::Work,
                objective: "fail this step".to_string(),
                target_files: vec![],
                dependencies: vec![],
                status: NodeStatus::Pending,
                attempt: 0,
                plan_depth: 0,
                model_tier: ModelTier::Cheap,
                summary: None,
                origin: NodeOrigin::Root,
            }],
            next_id: 0,
        };
        let output = run_machine(scheduler_handler(), SchedulerState::Running { graph });
        assert!(matches!(output, SchedulerOutput::Failed { .. }));
    }

    #[test]
    fn scheduler_output_includes_node_failure_reason() {
        let graph = RunGraph {
            nodes: vec![Node {
                id: NodeId("T".to_string()),
                kind: NodeKind::Work,
                objective: "fail this step".to_string(),
                target_files: vec![],
                dependencies: vec![],
                status: NodeStatus::Running,
                attempt: 0,
                plan_depth: 0,
                model_tier: ModelTier::Cheap,
                summary: None,
                origin: NodeOrigin::Root,
            }],
            next_id: 0,
        };

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("T".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("T".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "provider error (Retryable): connection refused".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "deliberation failed".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(reason.contains("deliberation failed"));
        assert!(reason.contains("provider error (Retryable): connection refused"));
    }

    #[test]
    fn scheduler_output_includes_integration_failure_reason() {
        let graph = RunGraph {
            nodes: vec![Node {
                id: NodeId("W".to_string()),
                kind: NodeKind::Work,
                objective: "integrate this step".to_string(),
                target_files: vec![],
                dependencies: vec![],
                status: NodeStatus::Integrating,
                attempt: 0,
                plan_depth: 0,
                model_tier: ModelTier::Cheap,
                summary: None,
                origin: NodeOrigin::Root,
            }],
            next_id: 0,
        };

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("W".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "validation failed: cargo test failed".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "integration failed".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(reason.contains("integration failed"));
        assert!(reason.contains("validation failed: cargo test failed"));
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
    fn three_node_chain_completes_via_handler() {
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };

        let output = run_machine(scheduler_handler(), SchedulerState::Running { graph });

        let SchedulerOutput::Complete { graph, .. } = output else {
            panic!("expected Complete")
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.status == NodeStatus::Completed)
        );
    }

    #[test]
    fn split_remaps_downstream_dependencies_and_chain_completes() {
        // A -> B -> C; B fails with Split on its first run.
        // After Split: B is Failed, a Plan node P is inserted, C's dependency is
        // rewritten from B to P. P completes (empty plan), then C completes.
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "do split", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };

        // Dispatch A.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching A")
        };

        // A completes: WorkAccepted → Integrating.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("A".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "A done".to_string(),
                }),
            },
        );
        let SchedulerState::Waiting { graph, running: _ } = t.state else {
            panic!("expected Waiting after A WorkAccepted")
        };

        // Integration succeeds → Running.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("A".to_string()),
                outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                    summary: "A integrated".to_string(),
                }),
            },
        );
        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after A integrates")
        };

        // Dispatch B.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching B")
        };

        // B fails with Split.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "task too complex".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose the work".to_string(),
                    },
                }),
            },
        );
        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after Split")
        };

        // Verify: original B is Failed.
        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        // Verify: split Plan node P exists with the right kind.
        let p = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("B-split-"))
            .expect("split Plan node");
        let split_id = p.id.clone();
        assert_eq!(p.kind, NodeKind::Plan);
        assert_eq!(p.status, NodeStatus::Pending);

        // Verify: C's dependency was rewritten from B to P.
        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&split_id),
            "C does not depend on split Plan node"
        );

        // Dispatch P (ready because A — P's inherited dependency — is Completed).
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching P")
        };

        // P completes as a Plan with no children.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: split_id.clone(),
            },
            SchedulerEvent::NodeReturned {
                node_id: split_id.clone(),
                outcome: NodeOutcome::PlanAccepted(PlanOutput { children: vec![] }),
            },
        );
        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after P completes")
        };

        // Dispatch C (now ready: P is Completed and C depends on P).
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching C")
        };

        // C completes: WorkAccepted → Integrating.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("C".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("C".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "C done".to_string(),
                }),
            },
        );
        let SchedulerState::Waiting { graph, running: _ } = t.state else {
            panic!("expected Waiting after C WorkAccepted")
        };

        // Integration succeeds → Running.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("C".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("C".to_string()),
                outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                    summary: "C integrated".to_string(),
                }),
            },
        );
        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after C integrates")
        };

        // All nodes terminal → scheduler reaches Complete.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Complete { graph } = t.state else {
            panic!("expected Complete, got non-Complete state")
        };

        // Final assertions.
        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert_eq!(c.status, NodeStatus::Completed);

        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);
    }

    #[test]
    fn full_chain_run() {
        let output = run_machine(
            scheduler_handler(),
            SchedulerState::Running {
                graph: chain_graph(),
            },
        );
        let SchedulerOutput::Complete { graph, .. } = output else {
            panic!("expected Complete")
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.status == NodeStatus::Completed)
        );
    }

    // ── Attempt-limit tests ───────────────────────────────────────────────────

    #[test]
    fn recovery_exhaustion_fails_scheduler() {
        // A node already at MAX_ATTEMPTS must not spawn a replacement regardless
        // of the recovery action; the scheduler transitions to Failed immediately.
        for (case, recovery) in [
            (
                "Retry",
                RecoveryAction::Retry {
                    message: "try again".to_string(),
                },
            ),
            (
                "ElevateModel",
                RecoveryAction::ElevateModel {
                    message: "escalate model".to_string(),
                },
            ),
            (
                "Split",
                RecoveryAction::Split {
                    message: "decompose the work".to_string(),
                },
            ),
        ] {
            let mut node = work_node("W", "failing task", &[]);
            node.attempt = MAX_ATTEMPTS;
            let graph = RunGraph {
                nodes: vec![node],
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
                        kind: FailureKind::DeliberationFailure,
                        message: "transient error".to_string(),
                        recovery,
                    }),
                },
            );

            let SchedulerState::Failed { graph, reason } = t.state else {
                panic!("[{case}] expected Failed, got {:#?}", t.state);
            };
            assert_eq!(
                graph.nodes.len(),
                1,
                "[{case}] no replacement node should be created"
            );
            assert_eq!(graph.nodes[0].status, NodeStatus::Failed, "[{case}]");
            assert!(
                reason.contains("exhausted"),
                "[{case}] reason should mention exhaustion, got: {reason:?}"
            );
            assert!(
                matches!(t.effects.as_slice(), [SchedulerEffect::ReturnFailed { .. }]),
                "[{case}]"
            );
        }
    }

    // ── Plan dependency validation tests ─────────────────────────────────────

    #[test]
    fn plan_with_unknown_dependency_fails_scheduler() {
        let graph = RunGraph {
            nodes: vec![plan_node("P", "plan something", &[])],
            next_id: 0,
        };
        let graph_before = running(graph, "P");
        let t = do_transition(
            SchedulerState::Waiting {
                graph: graph_before.clone(),
                running: NodeId("P".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("P".to_string()),
                outcome: NodeOutcome::PlanAccepted(PlanOutput {
                    children: vec![NodeRequest {
                        id: NodeId("child-1".to_string()),
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
                        target_files: vec![],
                        dependencies: vec![NodeId("missing".to_string())],
                    }],
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 1, "no children should be inserted");
        assert_eq!(graph.nodes[0].id, NodeId("P".to_string()));
        assert_eq!(
            graph.nodes[0].status,
            NodeStatus::Running,
            "plan node should be unchanged"
        );
        assert!(
            reason.contains("missing"),
            "reason should mention the missing id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn plan_with_valid_dependencies_still_succeeds() {
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
                        id: NodeId("child-1".to_string()),
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
                        target_files: vec![],
                        dependencies: vec![NodeId("P".to_string())],
                    }],
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 2, "child should be inserted");
        assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
        assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
    }

    #[test]
    fn sibling_dependencies_are_resolved_to_graph_ids() {
        // A plan output where B depends on A from the same batch.
        // Sibling deps are now supported: B's local dep on "A" must be
        // rewritten to the actual graph NodeId assigned to A on insertion.
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
                    children: vec![
                        NodeRequest {
                            id: NodeId("A".to_string()),
                            kind: NodeKind::Work,
                            objective: "step A".to_string(),
                            target_files: vec![],
                            dependencies: vec![],
                        },
                        NodeRequest {
                            id: NodeId("B".to_string()),
                            kind: NodeKind::Work,
                            objective: "step B".to_string(),
                            target_files: vec![],
                            dependencies: vec![NodeId("A".to_string())],
                        },
                    ],
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 3, "P + two children must be inserted");
        assert_eq!(
            graph.nodes[0].status,
            NodeStatus::Completed,
            "P must be Completed"
        );

        let child_a = graph
            .nodes
            .iter()
            .find(|n| n.objective == "step A")
            .expect("child A not found");
        let child_b = graph
            .nodes
            .iter()
            .find(|n| n.objective == "step B")
            .expect("child B not found");

        assert_eq!(child_b.dependencies.len(), 1);
        assert_eq!(
            child_b.dependencies[0], child_a.id,
            "B must depend on A's graph id"
        );
        assert_ne!(
            child_b.dependencies[0],
            NodeId("A".to_string()),
            "B must not retain the planner-local id"
        );
    }

    #[test]
    fn planner_can_create_two_work_nodes_with_dependency() {
        // End-to-end: a plan node expands into two work nodes where B depends
        // on A. Verify the scheduler runs A first, then B after A completes.
        let graph = RunGraph {
            nodes: vec![plan_node("root", "plan a two-step task", &[])],
            next_id: 0,
        };

        // Dispatch the root plan node.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching root");
        };

        // Root plan returns two tasks: write-tests (no deps) and implement (depends on write-tests).
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("root".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("root".to_string()),
                outcome: NodeOutcome::PlanAccepted(PlanOutput {
                    children: vec![
                        NodeRequest {
                            id: NodeId("write-tests".to_string()),
                            kind: NodeKind::Work,
                            objective: "write tests".to_string(),
                            target_files: vec![],
                            dependencies: vec![],
                        },
                        NodeRequest {
                            id: NodeId("implement".to_string()),
                            kind: NodeKind::Work,
                            objective: "implement feature".to_string(),
                            target_files: vec![],
                            dependencies: vec![NodeId("write-tests".to_string())],
                        },
                    ],
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after plan expansion");
        };
        assert_eq!(graph.nodes.len(), 3, "root + two children");

        // Only write-tests must be ready; implement is blocked on it.
        let ready = SchedulerMachine::find_ready(&graph);
        assert_eq!(ready.len(), 1, "only one node must be ready");
        let write_tests_id = ready[0].clone();
        let write_tests_node = graph.nodes.iter().find(|n| n.id == write_tests_id).unwrap();
        assert_eq!(write_tests_node.objective, "write tests");

        let implement_node = graph
            .nodes
            .iter()
            .find(|n| n.objective == "implement feature")
            .unwrap();
        assert!(
            implement_node.dependencies.contains(&write_tests_id),
            "implement must depend on write-tests graph id"
        );

        // Dispatch write-tests.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching write-tests");
        };

        // write-tests completes → Integrating.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: write_tests_id.clone(),
            },
            SchedulerEvent::NodeReturned {
                node_id: write_tests_id.clone(),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "tests written".to_string(),
                }),
            },
        );
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting (Integrating) after write-tests WorkAccepted");
        };

        // Integration succeeds → Running.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: write_tests_id.clone(),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: write_tests_id.clone(),
                outcome: crate::machines::scheduler::event::IntegrationOutcome::Succeeded(
                    crate::machines::scheduler::event::IntegrationOutput {
                        summary: "tests integrated".to_string(),
                    },
                ),
            },
        );
        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after write-tests integration");
        };

        // Now implement must be the only ready node.
        let ready = SchedulerMachine::find_ready(&graph);
        assert_eq!(ready.len(), 1, "implement must be the only ready node");
        let implement_id = ready[0].clone();
        let implement_node = graph.nodes.iter().find(|n| n.id == implement_id).unwrap();
        assert_eq!(implement_node.objective, "implement feature");

        // Dispatch implement.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting after dispatching implement");
        };

        // implement completes → Integrating.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: implement_id.clone(),
            },
            SchedulerEvent::NodeReturned {
                node_id: implement_id.clone(),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "feature implemented".to_string(),
                }),
            },
        );
        let SchedulerState::Waiting { graph, .. } = t.state else {
            panic!("expected Waiting (Integrating) after implement WorkAccepted");
        };

        // Integration succeeds → Running.
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: implement_id.clone(),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: implement_id.clone(),
                outcome: crate::machines::scheduler::event::IntegrationOutcome::Succeeded(
                    crate::machines::scheduler::event::IntegrationOutput {
                        summary: "implementation integrated".to_string(),
                    },
                ),
            },
        );
        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running after implement integration");
        };

        // All nodes terminal → Complete.
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Complete { graph } = t.state else {
            panic!("expected Complete, got {:#?}", t.state);
        };

        let completed: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .collect();
        assert_eq!(completed.len(), 3, "all three nodes must be Completed");
    }

    #[test]
    fn ordinary_missing_dependency_still_reports_unknown_node() {
        // A dependency that does not appear in the graph OR the current batch
        // should still produce the existing "unknown node id" diagnostic.
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
                        id: NodeId("child-1".to_string()),
                        kind: NodeKind::Work,
                        objective: "step".to_string(),
                        target_files: vec![],
                        dependencies: vec![NodeId("ghost".to_string())],
                    }],
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 1, "no children should be inserted");
        assert!(
            !reason.contains("same-batch sibling dependency"),
            "unknown dep should not be reported as sibling, got: {reason:?}"
        );
        assert!(
            reason.contains("ghost"),
            "reason should name the unknown id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn plan_expansion_respects_graph_size_limit() {
        let graph =
            graph_with_filler_nodes(plan_node("P", "plan something", &[]), MAX_GRAPH_NODES - 1);
        let graph = running(graph, "P");

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("P".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("P".to_string()),
                outcome: NodeOutcome::PlanAccepted(PlanOutput {
                    children: vec![
                        NodeRequest {
                            id: NodeId("child-1".to_string()),
                            kind: NodeKind::Work,
                            objective: "child one".to_string(),
                            target_files: vec![],
                            dependencies: vec![NodeId("P".to_string())],
                        },
                        NodeRequest {
                            id: NodeId("child-2".to_string()),
                            kind: NodeKind::Work,
                            objective: "child two".to_string(),
                            target_files: vec![],
                            dependencies: vec![NodeId("P".to_string())],
                        },
                    ],
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES - 1);
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| !matches!(node.origin, NodeOrigin::PlanExpansion)),
            "no children should be inserted"
        );
        assert!(reason.contains("graph size limit"));
        assert!(reason.contains(&MAX_GRAPH_NODES.to_string()));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { reason, .. }]
                if reason.contains("graph size limit")
                    && reason.contains(&MAX_GRAPH_NODES.to_string())
        ));
    }

    // ── Cancellation propagation tests ───────────────────────────────────────

    #[test]
    fn terminal_failure_cancels_downstream_chain() {
        // Graph: A -> B -> C -> D
        // A is already Completed, B is Running and fails terminally.
        // Expected final statuses: A=Completed, B=Failed, C=Cancelled, D=Cancelled.
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
                work_node("D", "step D", &["C"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "B"),
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "unrecoverable".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "fatal error".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };

        let status = |id: &str| {
            graph
                .nodes
                .iter()
                .find(|n| n.id.0 == id)
                .unwrap_or_else(|| panic!("node {id} not found"))
                .status
                .clone()
        };

        assert_eq!(status("A"), NodeStatus::Completed);
        assert_eq!(status("B"), NodeStatus::Failed);
        assert_eq!(status("C"), NodeStatus::Cancelled);
        assert_eq!(status("D"), NodeStatus::Cancelled);
    }

    #[test]
    fn recovery_respects_graph_size_limit() {
        // When the graph is already at MAX_GRAPH_NODES, any recovery action must
        // fail the scheduler rather than inserting a replacement node.
        #[derive(Clone, Copy)]
        enum RecoveryKind {
            Retry,
            Split,
            Elevate,
        }

        for (case, recovery_kind) in [
            ("Retry", RecoveryKind::Retry),
            ("Split", RecoveryKind::Split),
            ("Elevate", RecoveryKind::Elevate),
        ] {
            let recovery = match recovery_kind {
                RecoveryKind::Retry => RecoveryAction::Retry {
                    message: "try again".to_string(),
                },
                RecoveryKind::Split => RecoveryAction::Split {
                    message: "decompose the work".to_string(),
                },
                RecoveryKind::Elevate => RecoveryAction::ElevateModel {
                    message: "use stronger model".to_string(),
                },
            };

            let graph =
                graph_with_filler_nodes(work_node("W", "failing task", &[]), MAX_GRAPH_NODES);

            let t = do_transition(
                SchedulerState::Waiting {
                    graph: running(graph, "W"),
                    running: NodeId("W".to_string()),
                },
                SchedulerEvent::NodeReturned {
                    node_id: NodeId("W".to_string()),
                    outcome: NodeOutcome::Failed(NodeFailure {
                        kind: FailureKind::DeliberationFailure,
                        message: "transient error".to_string(),
                        recovery,
                    }),
                },
            );

            let SchedulerState::Failed { graph, reason } = t.state else {
                panic!("[{case}] expected Failed, got {:#?}", t.state);
            };
            assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES, "[{case}]");
            assert_eq!(graph.nodes[0].status, NodeStatus::Failed, "[{case}]");
            assert!(
                graph.nodes.iter().all(|node| match recovery_kind {
                    RecoveryKind::Retry => !matches!(node.origin, NodeOrigin::Retry { .. }),
                    RecoveryKind::Split => !matches!(node.origin, NodeOrigin::Split { .. }),
                    RecoveryKind::Elevate =>
                        !matches!(node.origin, NodeOrigin::ElevateModel { .. }),
                }),
                "[{case}] no replacement should be created"
            );
            assert!(
                reason.contains("graph size limit"),
                "[{case}] got: {reason:?}"
            );
            assert!(reason.contains(&MAX_GRAPH_NODES.to_string()), "[{case}]");
            assert!(
                matches!(t.effects.as_slice(), [SchedulerEffect::ReturnFailed { .. }]),
                "[{case}]"
            );
        }
    }

    // ── RecoverySummary / output classification tests ─────────────────────────

    #[test]
    fn clean_success_has_no_recovery() {
        let output = run_machine(
            scheduler_handler(),
            SchedulerState::Running {
                graph: single_work_graph(),
            },
        );
        let SchedulerOutput::Complete {
            recovery_summary, ..
        } = output
        else {
            panic!("expected Complete");
        };
        assert!(!recovery_summary.recovered);
        assert_eq!(recovery_summary.retry_count, 0);
        assert_eq!(recovery_summary.elevate_count, 0);
        assert_eq!(recovery_summary.split_count, 0);
    }

    #[test]
    fn split_success_reports_recovery() {
        // Construct a completed graph that reflects a Split recovery: the original
        // work node failed, a Split plan node replaced it and completed. We call
        // `output()` directly on the terminal state rather than using the stub
        // handler, since the stub would re-trigger Split on the plan node's
        // derived objective.
        let source_id = NodeId("S".to_string());
        let split_id = NodeId("S-split-0".to_string());
        let graph = RunGraph {
            nodes: vec![
                Node {
                    id: source_id.clone(),
                    kind: NodeKind::Work,
                    objective: "complex task".to_string(),
                    target_files: vec![],
                    dependencies: vec![],
                    status: NodeStatus::Failed,
                    attempt: 0,
                    plan_depth: 0,
                    model_tier: ModelTier::Cheap,
                    summary: None,
                    origin: NodeOrigin::Root,
                },
                Node {
                    id: split_id,
                    kind: NodeKind::Plan,
                    objective: "decompose complex task".to_string(),
                    target_files: vec![],
                    dependencies: vec![],
                    status: NodeStatus::Completed,
                    attempt: 1,
                    plan_depth: 1,
                    model_tier: ModelTier::Strong,
                    summary: Some("planned".to_string()),
                    origin: NodeOrigin::Split { source: source_id },
                },
            ],
            next_id: 1,
        };
        let state = SchedulerState::Complete { graph };
        let output = SchedulerMachine {
            has_strong_tier: true,
        }
        .output(&state)
        .expect("Complete is a terminal state");
        let SchedulerOutput::Complete {
            recovery_summary, ..
        } = output
        else {
            panic!("expected Complete");
        };
        assert!(recovery_summary.recovered);
        assert_eq!(recovery_summary.retry_count, 0);
        assert_eq!(recovery_summary.elevate_count, 0);
        assert_eq!(recovery_summary.split_count, 1);
    }

    #[test]
    fn split_below_attempt_limit_still_creates_plan_node() {
        // A node at attempt 0 (below MAX_ATTEMPTS) must still produce a Split
        // Plan node with attempt incremented to 1, and must remap downstream deps.
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("W", "complex task", &["A"]),
                work_node("C", "step C", &["W"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "task too complex".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose the work".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };

        // Original W is Failed.
        let w = graph.nodes.iter().find(|n| n.id.0 == "W").expect("W");
        assert_eq!(w.status, NodeStatus::Failed);

        // Split Plan node exists with attempt=1 and Strong tier.
        let split = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("W-split-"))
            .expect("split Plan node");
        assert_eq!(split.kind, NodeKind::Plan);
        assert_eq!(split.status, NodeStatus::Pending);
        assert_eq!(split.attempt, 1, "split Plan node must carry attempt + 1");
        assert_eq!(split.model_tier, ModelTier::Strong);

        // C's dependency was rewritten from W to the split Plan node.
        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("W".to_string())),
            "C must not depend on failed W"
        );
        assert!(
            c.dependencies.contains(&split.id),
            "C must depend on the split Plan node"
        );
    }

    #[test]
    fn split_depth_limit_fails_scheduler() {
        let mut graph = RunGraph {
            nodes: vec![work_node("W", "complex task", &[])],
            next_id: 0,
        };
        graph.nodes[0].plan_depth = MAX_PLAN_DEPTH;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "task too complex".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose the work".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 1, "must not insert split plan");
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            reason.contains("plan depth limit") && reason.contains(&MAX_PLAN_DEPTH.to_string()),
            "unexpected reason: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { reason, .. }]
                if reason.contains("plan depth limit")
                    && reason.contains(&MAX_PLAN_DEPTH.to_string())
        ));
    }

    #[test]
    fn integration_failure_retry_routes_to_replacement() {
        // Graph: A -> B -> C; B is Integrating (work accepted, integration pending).
        // Integration fails with Retry.
        // Expected:
        //   - original B becomes Failed
        //   - replacement B' is created with the same kind/objective
        //   - B'.attempt == 1, B'.dependencies == B.dependencies
        //   - C's dependency is remapped from B to B'
        //   - scheduler returns to Running (no panic)
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;
        graph.nodes[1].status = NodeStatus::Integrating;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("B".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "integration error".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "retry after integration failure".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };

        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        let b_prime = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("B-retry-"))
            .expect("B' replacement");
        assert_eq!(b_prime.kind, NodeKind::Work);
        assert_eq!(b_prime.objective, "step B");
        assert_eq!(b_prime.attempt, 1);
        assert_eq!(b_prime.status, NodeStatus::Pending);
        assert_eq!(b_prime.dependencies, vec![NodeId("A".to_string())]);

        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&b_prime.id),
            "C does not depend on B'"
        );

        assert!(t.effects.is_empty());
    }

    #[test]
    fn integration_failure_elevate_routes_to_strong_replacement() {
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;
        graph.nodes[1].status = NodeStatus::Integrating;
        graph.nodes[1].attempt = 1;
        let b_attempt = graph.nodes[1].attempt;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("B".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "integration error".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use stronger model".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };

        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        let b_prime = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("B-elevated-"))
            .expect("B' replacement");
        assert_eq!(b_prime.kind, NodeKind::Work);
        assert_eq!(b_prime.model_tier, ModelTier::Strong);
        assert_eq!(b_prime.attempt, b_attempt + 1);

        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&b_prime.id),
            "C does not depend on B'"
        );

        assert!(t.effects.is_empty());
    }

    #[test]
    fn integration_failure_split_routes_to_plan_replacement() {
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;
        graph.nodes[1].status = NodeStatus::Integrating;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("B".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "integration error".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose step B".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };

        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        let replacement = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("B-split-"))
            .expect("split replacement");
        assert_eq!(replacement.kind, NodeKind::Plan);
        assert_eq!(replacement.model_tier, ModelTier::Strong);
        assert!(matches!(
            &replacement.origin,
            NodeOrigin::Split { source } if *source == NodeId("B".to_string())
        ));

        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&replacement.id),
            "C does not depend on split replacement"
        );

        assert!(t.effects.is_empty());
    }

    #[test]
    fn integration_failure_terminal_cancels_downstream_dependents() {
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
                work_node("D", "step D", &["C"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;
        graph.nodes[1].status = NodeStatus::Integrating;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("B".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "integration error".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "integration cannot be recovered".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };

        let status = |id: &str| {
            graph
                .nodes
                .iter()
                .find(|n| n.id.0 == id)
                .unwrap_or_else(|| panic!("node {id} not found"))
                .status
                .clone()
        };

        assert_eq!(status("B"), NodeStatus::Failed);
        assert_eq!(status("C"), NodeStatus::Cancelled);
        assert_eq!(status("D"), NodeStatus::Cancelled);
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn integration_failure_exhaustion_fails_scheduler() {
        // A node in Integrating status at MAX_ATTEMPTS must not spawn a replacement
        // regardless of the recovery action; the scheduler transitions to Failed.
        for (case, recovery) in [
            (
                "Retry",
                RecoveryAction::Retry {
                    message: "retry integration".to_string(),
                },
            ),
            (
                "ElevateModel",
                RecoveryAction::ElevateModel {
                    message: "use stronger model".to_string(),
                },
            ),
            (
                "Split",
                RecoveryAction::Split {
                    message: "decompose step B".to_string(),
                },
            ),
        ] {
            let mut node = work_node("B", "step B", &[]);
            node.status = NodeStatus::Integrating;
            node.attempt = MAX_ATTEMPTS;
            let graph = RunGraph {
                nodes: vec![node],
                next_id: 0,
            };

            let t = do_transition(
                SchedulerState::Waiting {
                    graph,
                    running: NodeId("B".to_string()),
                },
                SchedulerEvent::IntegrationReturned {
                    node_id: NodeId("B".to_string()),
                    outcome: IntegrationOutcome::Failed(IntegrationFailure {
                        kind: FailureKind::IntegrationFailure,
                        message: "integration error".to_string(),
                        recovery,
                    }),
                },
            );

            let SchedulerState::Failed { graph, reason } = t.state else {
                panic!("[{case}] expected Failed, got {:#?}", t.state);
            };
            assert_eq!(
                graph.nodes.len(),
                1,
                "[{case}] no replacement should be created"
            );
            assert_eq!(graph.nodes[0].status, NodeStatus::Failed, "[{case}]");
            assert!(
                reason.contains("exhausted"),
                "[{case}] reason should mention exhaustion, got: {reason:?}"
            );
            assert!(
                matches!(t.effects.as_slice(), [SchedulerEffect::ReturnFailed { .. }]),
                "[{case}]"
            );
        }
    }

    // ── Deadlock diagnostics tests ────────────────────────────────────────────

    #[test]
    fn no_ready_reports_missing_dependency() {
        // C is Pending and depends on B, but B does not exist in the graph.
        let graph = RunGraph {
            nodes: vec![work_node("C", "do C", &["B"])],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("missing dependency"),
            "reason should mention missing dependency, got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain the missing node id, got: {reason:?}"
        );
    }

    #[test]
    fn no_ready_reports_blocked_or_possible_cycle() {
        // A depends on B, B depends on A — neither can ever become ready.
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "do A", &["B"]),
                work_node("B", "do B", &["A"]),
            ],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("blocked") || reason.contains("cycle"),
            "reason should mention blocked or cycle, got: {reason:?}"
        );
    }

    // ── Graph invariant validation tests ─────────────────────────────────────

    #[test]
    fn duplicate_node_ids_fail_graph_validation() {
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "first task", &[]),
                work_node("A", "second task", &[]),
            ],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("duplicate node id"),
            "reason should mention duplicate node id, got: {reason:?}"
        );
        assert!(
            reason.contains('A'),
            "reason should contain the duplicated id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn missing_dependency_fails_graph_validation() {
        let graph = RunGraph {
            nodes: vec![work_node("A", "do something", &["ghost"])],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);
        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("missing dependency"),
            "reason should mention missing dependency, got: {reason:?}"
        );
        assert!(
            reason.contains("ghost"),
            "reason should contain the missing dependency id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn graph_validation_does_not_parse_node_ids() {
        let graph = RunGraph {
            nodes: vec![
                work_node("root", "root task", &[]),
                work_node("task-999", "numeric-looking task", &["root"]),
                work_node("custom-123", "custom task", &["task-999"]),
            ],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Waiting { graph, running } = t.state else {
            panic!("expected Waiting, got {:#?}", t.state);
        };
        assert_eq!(running, NodeId("root".to_string()));
        assert_eq!(graph.next_id, 0);
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::RunNode {
                node_id,
                ..
            }] if *node_id == NodeId("root".to_string())
        ));
    }

    #[test]
    fn retry_origin_with_missing_source_fails_validation() {
        let mut node_b = work_node("B", "retry task", &[]);
        node_b.origin = NodeOrigin::Retry {
            source: NodeId("missing".to_string()),
        };
        let graph = RunGraph {
            nodes: vec![node_b],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("missing origin source"),
            "reason should contain 'missing origin source', got: {reason:?}"
        );
        assert!(
            reason.contains("Retry"),
            "reason should mention Retry, got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain node id B, got: {reason:?}"
        );
        assert!(
            reason.contains("missing"),
            "reason should contain missing source id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn elevate_origin_with_missing_source_fails_validation() {
        let mut node_b = work_node("B", "elevate task", &[]);
        node_b.origin = NodeOrigin::ElevateModel {
            source: NodeId("missing".to_string()),
        };
        let graph = RunGraph {
            nodes: vec![node_b],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("missing origin source"),
            "reason should contain 'missing origin source', got: {reason:?}"
        );
        assert!(
            reason.contains("ElevateModel"),
            "reason should mention ElevateModel, got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain node id B, got: {reason:?}"
        );
        assert!(
            reason.contains("missing"),
            "reason should contain missing source id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn split_origin_with_missing_source_fails_validation() {
        let mut node_b = work_node("B", "split task", &[]);
        node_b.origin = NodeOrigin::Split {
            source: NodeId("missing".to_string()),
        };
        let graph = RunGraph {
            nodes: vec![node_b],
            next_id: 0,
        };
        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("missing origin source"),
            "reason should contain 'missing origin source', got: {reason:?}"
        );
        assert!(
            reason.contains("Split"),
            "reason should mention Split, got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain node id B, got: {reason:?}"
        );
        assert!(
            reason.contains("missing"),
            "reason should contain missing source id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    // ── Outcome/phase validation tests ───────────────────────────────────────

    #[test]
    fn plan_node_rejects_work_accepted() {
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
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "work done".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("Plan"),
            "reason should mention Plan, got: {reason:?}"
        );
        assert!(
            reason.contains("WorkAccepted"),
            "reason should mention WorkAccepted, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn work_node_rejects_plan_accepted() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "A"),
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("A".to_string()),
                outcome: NodeOutcome::PlanAccepted(PlanOutput { children: vec![] }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("Work"),
            "reason should mention Work, got: {reason:?}"
        );
        assert!(
            reason.contains("PlanAccepted"),
            "reason should mention PlanAccepted, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn node_returned_rejects_integrating_node() {
        // Waiting { running: B } with B status = Integrating.
        // NodeReturned must be rejected: it is for the execution phase only.
        let mut graph = RunGraph {
            nodes: vec![work_node("B", "do work", &[])],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Integrating;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "spurious result".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("protocol violation"),
            "reason should contain 'protocol violation', got: {reason:?}"
        );
        assert!(
            reason.contains("NodeReturned"),
            "reason should contain 'NodeReturned', got: {reason:?}"
        );
        assert!(
            reason.contains("Running"),
            "reason should mention expected status Running, got: {reason:?}"
        );
        assert!(
            reason.contains("Integrating"),
            "reason should mention actual status Integrating, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn integration_returned_rejects_non_integrating_work() {
        // Work node is Running (not Integrating) when IntegrationReturned arrives.
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "A"),
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("A".to_string()),
                outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                    summary: "done".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("Integrating"),
            "reason should mention Integrating, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn integration_returned_rejects_plan_node() {
        let graph = RunGraph {
            nodes: vec![plan_node("P", "plan something", &[])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "P"),
                running: NodeId("P".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("P".to_string()),
                outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                    summary: "done".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("Work") || reason.contains("Plan"),
            "reason should mention Work or Plan, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    // ── Protocol violation tests ──────────────────────────────────────────────

    #[test]
    fn node_returned_wrong_node_fails_scheduler() {
        let graph = RunGraph {
            nodes: vec![work_node("A", "task A", &[])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "A"),
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "spurious result".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("protocol violation"),
            "reason should contain 'protocol violation', got: {reason:?}"
        );
        assert!(
            reason.contains('A'),
            "reason should contain expected node A, got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain received node B, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn integration_returned_wrong_node_fails_scheduler() {
        let mut graph = RunGraph {
            nodes: vec![work_node("A", "task A", &[])],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Integrating;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("B".to_string()),
                outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                    summary: "spurious result".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("protocol violation"),
            "reason should contain 'protocol violation', got: {reason:?}"
        );
        assert!(
            reason.contains('A'),
            "reason should contain expected node A, got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain received node B, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn running_rejects_node_returned() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Running { graph },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("A".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "spurious".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("protocol violation"),
            "reason should contain 'protocol violation', got: {reason:?}"
        );
        assert!(
            reason.contains("Running"),
            "reason should mention Running, got: {reason:?}"
        );
        assert!(
            reason.contains("NodeReturned"),
            "reason should mention NodeReturned, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn running_rejects_integration_returned() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Running { graph },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("A".to_string()),
                outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                    summary: "spurious".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("protocol violation"),
            "reason should contain 'protocol violation', got: {reason:?}"
        );
        assert!(
            reason.contains("Running"),
            "reason should mention Running, got: {reason:?}"
        );
        assert!(
            reason.contains("IntegrationReturned"),
            "reason should mention IntegrationReturned, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn waiting_rejects_start() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::Start,
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("protocol violation"),
            "reason should contain 'protocol violation', got: {reason:?}"
        );
        assert!(
            reason.contains("Waiting"),
            "reason should mention Waiting, got: {reason:?}"
        );
        assert!(
            reason.contains("Start"),
            "reason should mention Start, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    // ── Waiting-state invariant validation tests ──────────────────────────────

    #[test]
    fn waiting_with_missing_running_node_fails() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("missing".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("missing".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "irrelevant".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("invalid waiting state"),
            "reason should contain 'invalid waiting state', got: {reason:?}"
        );
        assert!(
            reason.contains("missing"),
            "reason should contain the missing node id, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn waiting_with_completed_running_node_fails() {
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "done", &[]),
                work_node("B", "also done", &["A"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;
        graph.nodes[1].status = NodeStatus::Completed;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "irrelevant".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("invalid waiting state"),
            "reason should contain 'invalid waiting state', got: {reason:?}"
        );
        assert!(
            reason.contains("Completed"),
            "reason should contain the actual status, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn waiting_with_running_node_still_works() {
        let graph = single_work_graph();
        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "A"),
                running: NodeId("A".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("A".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "success".to_string(),
                }),
            },
        );

        assert!(
            matches!(t.state, SchedulerState::Waiting { .. }),
            "expected Waiting (Integrating), got {:#?}",
            t.state
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::IntegrateWork { .. }]
        ));
    }

    // ── Serial active-node invariant tests ───────────────────────────────────

    #[test]
    fn running_state_rejects_preexisting_active_node() {
        let mut graph = single_work_graph();
        graph.nodes[0].status = NodeStatus::Running;

        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("invalid running state"),
            "reason should contain 'invalid running state', got: {reason:?}"
        );
        assert!(
            reason.contains('A'),
            "reason should contain the node id, got: {reason:?}"
        );
        assert!(
            reason.contains("Running"),
            "reason should contain the status, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn running_state_rejects_preexisting_integrating_node() {
        let mut graph = single_work_graph();
        graph.nodes[0].status = NodeStatus::Integrating;

        let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("invalid running state"),
            "reason should contain 'invalid running state', got: {reason:?}"
        );
        assert!(
            reason.contains('A'),
            "reason should contain the node id, got: {reason:?}"
        );
        assert!(
            reason.contains("Integrating"),
            "reason should contain the status, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn waiting_state_rejects_no_active_nodes() {
        // B exists but is Pending — no active node in the graph.
        let graph = RunGraph {
            nodes: vec![work_node("B", "do B", &[])],
            next_id: 0,
        };
        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "irrelevant".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("invalid waiting state"),
            "reason should contain 'invalid waiting state', got: {reason:?}"
        );
        assert!(
            reason.contains("found none") || reason.contains("Pending"),
            "reason should mention 'found none' or equivalent status, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn waiting_state_rejects_multiple_active_nodes() {
        // B and C are both active — violates the serial invariant.
        let mut graph = RunGraph {
            nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Running;
        graph.nodes[1].status = NodeStatus::Running;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "irrelevant".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("multiple active nodes"),
            "reason should contain 'multiple active nodes', got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain node id B, got: {reason:?}"
        );
        assert!(
            reason.contains('C'),
            "reason should contain node id C, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn waiting_state_rejects_active_node_that_differs_from_running() {
        // C is active, B is non-active — the active node doesn't match waiting.running.
        let mut graph = RunGraph {
            nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
            next_id: 0,
        };
        graph.nodes[1].status = NodeStatus::Running;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::WorkAccepted(WorkOutput {
                    summary: "irrelevant".to_string(),
                }),
            },
        );

        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert!(
            reason.contains("invalid waiting state"),
            "reason should contain 'invalid waiting state', got: {reason:?}"
        );
        assert!(
            reason.contains('B'),
            "reason should contain waiting.running id B, got: {reason:?}"
        );
        assert!(
            reason.contains('C'),
            "reason should contain active node id C, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    // ── Model-tier policy tests ───────────────────────────────────────────────

    #[test]
    fn single_tier_elevate_falls_back_to_retry() {
        // has_strong_tier: false → ElevateModel must not create a Strong replacement;
        // it must fall back to Retry, preserving the original model tier.
        let graph = RunGraph {
            nodes: vec![work_node("W", "do elevate", &[])],
            next_id: 0,
        };
        let t = SchedulerMachine {
            has_strong_tier: false,
        }
        .transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "needs stronger model".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use strong".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 2, "must create a replacement node");
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        let replacement = &graph.nodes[1];
        assert!(
            matches!(replacement.origin, NodeOrigin::Retry { .. }),
            "single-tier ElevateModel must fall back to Retry, got origin: {:?}",
            replacement.origin
        );
        assert_eq!(
            replacement.model_tier,
            ModelTier::Cheap,
            "fallback Retry must preserve the original Cheap tier"
        );
        assert!(t.effects.is_empty());
    }

    #[test]
    fn multi_tier_elevate_creates_strong_replacement() {
        // has_strong_tier: true → ElevateModel on a Cheap-tier node must produce a
        // Strong-tier replacement.
        let graph = RunGraph {
            nodes: vec![work_node("W", "do elevate", &[])],
            next_id: 0,
        };
        let t = SchedulerMachine {
            has_strong_tier: true,
        }
        .transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "needs stronger model".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use strong".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert_eq!(graph.nodes.len(), 2);
        let replacement = &graph.nodes[1];
        assert_eq!(replacement.model_tier, ModelTier::Strong);
        assert!(
            matches!(replacement.origin, NodeOrigin::ElevateModel { .. }),
            "multi-tier must produce ElevateModel replacement"
        );
    }

    #[test]
    fn single_tier_elevate_exhausted_gives_clear_terminal_failure() {
        // has_strong_tier: false + MAX_ATTEMPTS → Terminal with "no higher model tier available"
        // in the reason string.
        let mut node = work_node("W", "hard task", &[]);
        node.attempt = MAX_ATTEMPTS;
        let graph = RunGraph {
            nodes: vec![node],
            next_id: 0,
        };
        let t = SchedulerMachine {
            has_strong_tier: false,
        }
        .transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "capability ceiling".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "escalate model".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 1, "no replacement should be created");
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            reason.contains("no higher model tier available"),
            "reason must mention no higher model tier, got: {reason:?}"
        );
        assert!(
            reason.contains("exhausted") || reason.contains(&MAX_ATTEMPTS.to_string()),
            "reason must mention attempt exhaustion, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn elevate_at_strong_tier_falls_back_to_retry() {
        // A node already running at ModelTier::Strong has no higher tier to go to
        // even with has_strong_tier: true. Must fall back to Retry.
        let mut node = work_node("W", "hard task at strong", &[]);
        node.model_tier = ModelTier::Strong;
        let graph = RunGraph {
            nodes: vec![node],
            next_id: 0,
        };
        let t = SchedulerMachine {
            has_strong_tier: true,
        }
        .transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "still failing at strong tier".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use even stronger".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), 2, "must create a Retry replacement");
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        let replacement = &graph.nodes[1];
        assert!(
            matches!(replacement.origin, NodeOrigin::Retry { .. }),
            "Strong-tier node with ElevateModel must fall back to Retry"
        );
    }

    #[test]
    fn terminal_failure_does_not_touch_completed_nodes() {
        // Graph: A -> B -> C
        // A is Completed, B is Running and fails terminally.
        // A must remain Completed; only C (Pending) should be Cancelled.
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "B"),
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("B".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "unrecoverable".to_string(),
                    recovery: RecoveryAction::Terminal {
                        message: "fatal error".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, .. } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };

        let a = graph.nodes.iter().find(|n| n.id.0 == "A").unwrap();
        let b = graph.nodes.iter().find(|n| n.id.0 == "B").unwrap();
        let c = graph.nodes.iter().find(|n| n.id.0 == "C").unwrap();

        assert_eq!(a.status, NodeStatus::Completed, "A must remain Completed");
        assert_eq!(b.status, NodeStatus::Failed);
        assert_eq!(c.status, NodeStatus::Cancelled);
    }
}
