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
#[path = "machine_tests/mod.rs"]
mod machine_tests;
