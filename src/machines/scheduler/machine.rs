//! Scheduler machine — state-machine entry point.
//!
//! Owns `SchedulerMachine`, `SchedulerTerminalOutput`, `RecoverySummary`, and the
//! `transition` and terminal-output functions. Pure graph helpers live in
//! `graph.rs`; recovery routing and application live in `recovery.rs`.
//!
//! The transition function implements:
//!
//! ```text
//! (SchedulerState, SchedulerEvent) -> (SchedulerState, SchedulerEffect)
//! ```

use crate::engine::Transition;

use super::RunConfig;
use super::effect::SchedulerEffect;
use super::event::SchedulerEvent;
use super::failure::FailureReason;
use super::graph::{ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph};
use super::request::RunRequest;
use super::state::SchedulerState;
use super::types::{IntegrationFailure, NodeFailure};
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
pub enum SchedulerTerminalOutput {
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
        /// The typed cause of the failure.
        reason: FailureReason,
    },
}

/// The scheduler state machine.
///
/// All durable run data travels inside `SchedulerState`, including the
/// `RunConfig` policy. This struct has no fields; it exists only as the
/// namespace for `transition`, `output`, and `initial_state`.
pub struct SchedulerMachine;

impl SchedulerMachine {
    /// Build the initial scheduler state from a run request and policy config.
    ///
    /// Creates a `SchedulerState::Active` containing a single root `Plan`
    /// node whose objective is taken from the request. The `run_config` is
    /// embedded in the state so `transition` is fully reproducible from
    /// `(state, event)`.
    pub fn initial_state(request: RunRequest, run_config: RunConfig) -> SchedulerState {
        let root = Node {
            id: graph::new_node_id(),
            kind: NodeKind::Plan,
            worker_role: None,
            objective: request.objective,
            target_files: vec![],
            required_validation_targets: vec![],
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
            retry_feedback: None,
        };
        SchedulerState::Active {
            graph: RunGraph { nodes: vec![root] },
            run_config,
        }
    }

    // All graph helpers live in graph.rs and recovery.rs.
}

#[cfg(test)]
impl SchedulerMachine {
    pub(super) fn find_ready(g: &RunGraph) -> Vec<NodeId> {
        g.find_ready()
    }
}

impl SchedulerMachine {
    /// Returns the event used to bootstrap the scheduler on the first tick.
    pub fn start_event(&self) -> SchedulerEvent {
        SchedulerEvent::Start
    }

    /// Pure transition function: given the current state and an event, returns
    /// the next state and any effects to dispatch.
    ///
    /// The outcome depends only on `(state, event)`. `RunConfig` is read from
    /// the state variant itself rather than from any field on `SchedulerMachine`.
    pub fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        match (state, event) {
            // Scan the graph, then in the same tick either complete, fail, or dispatch.
            //
            // Three outcomes:
            //   1. All nodes are terminal → enter Complete and stop.
            //   2. Some nodes are Pending but none are ready → deadlock; enter Failed.
            //   3. At least one node is ready → mark it Running, emit RunNode, move to Waiting.
            (SchedulerState::Active { graph, run_config }, SchedulerEvent::Start) => {
                if let Err(detail) = graph.validate_graph_invariants() {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: FailureReason::GraphInvariantViolation(detail),
                        },
                        effects: vec![],
                    };
                }
                let active = graph.active_nodes();
                if let Some(node) = active.first() {
                    let detail = format!(
                        "invalid running state: node {} is {:?}",
                        node.id.0, node.status
                    );
                    return recovery::failed_transition(
                        graph,
                        FailureReason::ProtocolViolation(detail),
                    );
                }
                if graph.all_complete() {
                    if let Err(detail) = graph.validate_required_tests_completed() {
                        Transition {
                            state: SchedulerState::Failed {
                                graph: graph.clone(),
                                reason: FailureReason::RequiredTestTargetsMissing(detail),
                            },
                            effects: vec![],
                        }
                    } else {
                        Transition {
                            state: SchedulerState::Complete {
                                graph: graph.clone(),
                            },
                            effects: vec![],
                        }
                    }
                } else {
                    let ready = graph.find_ready();
                    if ready.is_empty() {
                        let detail = graph.diagnose_no_ready();
                        Transition {
                            state: SchedulerState::Failed {
                                graph: graph.clone(),
                                reason: FailureReason::Deadlock(detail),
                            },
                            effects: vec![],
                        }
                    } else {
                        let node_id = ready[0].clone();
                        let (
                            kind,
                            worker_role,
                            objective,
                            target_files,
                            test_plan_context,
                            model_tier,
                            attempt,
                            retry_feedback,
                        ) = {
                            let n = graph.get_node(&node_id);
                            (
                                n.kind.clone(),
                                n.worker_role.clone(),
                                n.objective.clone(),
                                n.target_files.clone(),
                                graph.test_plan_context_for_node(&node_id),
                                n.model_tier,
                                n.attempt,
                                n.retry_feedback.clone(),
                            )
                        };
                        let effect = SchedulerEffect::RunNode {
                            node_id: node_id.clone(),
                            worker_role,
                            kind,
                            objective,
                            target_files,
                            test_plan_context,
                            model_tier,
                            attempt,
                            retry_feedback,
                        };
                        let graph = graph.mark_node(&node_id, NodeStatus::Running);
                        Transition {
                            state: SchedulerState::Waiting { graph, run_config },
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
                SchedulerState::Waiting { graph, run_config },
                event @ (SchedulerEvent::PlanAccepted { .. }
                | SchedulerEvent::WorkAccepted { .. }
                | SchedulerEvent::NodeFailed { .. }),
            ) => {
                let node_id = node_event_id(&event).clone();
                let active_node = match graph.active_node() {
                    Ok(node) => node.id.clone(),
                    Err(detail) => {
                        return recovery::failed_transition(
                            graph,
                            FailureReason::ProtocolViolation(detail),
                        );
                    }
                };

                if active_node != node_id {
                    let detail = format!(
                        "expected result for node {} but received {}",
                        active_node.0, node_id.0
                    );
                    return recovery::failed_transition(
                        graph,
                        FailureReason::ProtocolViolation(detail),
                    );
                }

                // Validate that the node is in Running status (not Integrating or other).
                if let Some(detail) = graph.invalid_node_return_reason(&node_id) {
                    return recovery::failed_transition(
                        graph,
                        FailureReason::ProtocolViolation(detail),
                    );
                }

                // Validate that the outcome is compatible with the node's kind.
                let node_kind = graph.get_node(&node_id).kind.clone();
                if let Some(detail) = graph::invalid_node_event_reason(&node_id, &node_kind, &event)
                {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: FailureReason::ProtocolViolation(detail),
                        },
                        effects: vec![],
                    };
                }

                match event {
                    // A successful planner expands the graph: the plan node is marked
                    // Completed and its requested children are inserted as new Pending
                    // nodes. The scheduler then re-scans for ready nodes.
                    //
                    // Validation runs first, before any mutation, so an invalid plan
                    // does not insert children. A plan-depth violation additionally
                    // marks the original plan Failed as the circuit breaker source.
                    SchedulerEvent::PlanAccepted { plan, .. } => {
                        let parent_depth = graph.get_node(&node_id).plan_depth;
                        match graph.validate_plan_dependencies(&plan.children) {
                            Err(detail) => Transition {
                                state: SchedulerState::Failed {
                                    graph: graph.clone(),
                                    reason: FailureReason::GraphInvariantViolation(detail),
                                },
                                effects: vec![],
                            },
                            Ok(()) if !graph.graph_has_capacity(plan.children.len()) => {
                                Transition {
                                    state: SchedulerState::Failed {
                                        graph: graph.clone(),
                                        reason: FailureReason::GraphCapacityExceeded {
                                            limit: graph::MAX_GRAPH_NODES,
                                        },
                                    },
                                    effects: vec![],
                                }
                            }
                            Ok(()) => {
                                if graph::validate_plan_child_depths(parent_depth, &plan.children)
                                    .is_err()
                                {
                                    let graph = graph.mark_node(&node_id, NodeStatus::Failed);
                                    return recovery::failed_transition(
                                        graph,
                                        FailureReason::PlanDepthExceeded {
                                            limit: graph::MAX_PLAN_DEPTH,
                                        },
                                    );
                                }
                                let graph = graph.mark_node(&node_id, NodeStatus::Completed);
                                let graph = graph.insert_children(&node_id, plan.children);
                                Transition {
                                    state: SchedulerState::Active { graph, run_config },
                                    effects: vec![],
                                }
                            }
                        }
                    }

                    // Work accepted: the node moves to Integrating and an IntegrateWork
                    // effect is emitted. The node is not yet dependency-satisfying; that
                    // only happens when IntegrationSucceeded arrives.
                    SchedulerEvent::WorkAccepted { work, .. } => {
                        let (objective, target_files, validation_plan, attempt) = {
                            let node = graph.get_node(&node_id);
                            (
                                node.objective.clone(),
                                node.target_files.clone(),
                                node.validation_plan.clone(),
                                node.attempt,
                            )
                        };
                        let graph = graph.mark_node(&node_id, NodeStatus::Integrating);
                        Transition {
                            state: SchedulerState::Waiting { graph, run_config },
                            effects: vec![SchedulerEffect::IntegrateWork {
                                node_id,
                                objective,
                                work,
                                attempt,
                                target_files,
                                validation_plan,
                            }],
                        }
                    }

                    SchedulerEvent::NodeFailed {
                        failure:
                            NodeFailure {
                                kind,
                                message,
                                recovery,
                            },
                        ..
                    } => recovery::route_recovery(
                        run_config, graph, &node_id, kind, message, recovery,
                    ),
                    _ => unreachable!("node event group matched above"),
                }
            }

            // Integration finished: success marks the node Completed and
            // resumes scanning; failure routes through the same recovery
            // machinery as execution failure.
            (
                SchedulerState::Waiting { graph, run_config },
                event @ (SchedulerEvent::IntegrationSucceeded { .. }
                | SchedulerEvent::IntegrationFailed { .. }),
            ) => {
                let node_id = integration_event_id(&event).clone();
                let active_node = match graph.active_node() {
                    Ok(node) => node.id.clone(),
                    Err(detail) => {
                        return recovery::failed_transition(
                            graph,
                            FailureReason::ProtocolViolation(detail),
                        );
                    }
                };

                if active_node != node_id {
                    let detail = format!(
                        "expected integration result for node {} but received {}",
                        active_node.0, node_id.0
                    );
                    return recovery::failed_transition(
                        graph,
                        FailureReason::ProtocolViolation(detail),
                    );
                }

                // Validate that integration arrives for a Work node in Integrating status.
                if let Some(detail) = graph.invalid_integration_reason(&node_id) {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: FailureReason::ProtocolViolation(detail),
                        },
                        effects: vec![],
                    };
                }

                match event {
                    SchedulerEvent::IntegrationSucceeded {
                        output: integration_output,
                        ..
                    } => {
                        let graph = graph
                            .mark_node_completed_with_summary(&node_id, integration_output.summary);
                        Transition {
                            state: SchedulerState::Active { graph, run_config },
                            effects: vec![],
                        }
                    }
                    SchedulerEvent::IntegrationFailed {
                        failure:
                            IntegrationFailure {
                                kind,
                                message,
                                recovery,
                            },
                        ..
                    } => recovery::route_recovery(
                        run_config, graph, &node_id, kind, message, recovery,
                    ),
                    _ => unreachable!("integration event group matched above"),
                }
            }

            (
                SchedulerState::Active { graph, .. },
                SchedulerEvent::PlanAccepted { .. }
                | SchedulerEvent::WorkAccepted { .. }
                | SchedulerEvent::NodeFailed { .. },
            ) => recovery::failed_transition(
                graph,
                FailureReason::ProtocolViolation(
                    "state Active cannot consume NodeReturned".to_string(),
                ),
            ),

            (
                SchedulerState::Active { graph, .. },
                SchedulerEvent::IntegrationSucceeded { .. }
                | SchedulerEvent::IntegrationFailed { .. },
            ) => recovery::failed_transition(
                graph,
                FailureReason::ProtocolViolation(
                    "state Active cannot consume IntegrationReturned".to_string(),
                ),
            ),

            (SchedulerState::Waiting { graph, .. }, SchedulerEvent::Start) => {
                recovery::failed_transition(
                    graph,
                    FailureReason::ProtocolViolation(
                        "state Waiting cannot consume Start".to_string(),
                    ),
                )
            }

            (SchedulerState::Complete { graph } | SchedulerState::Failed { graph, .. }, _) => {
                recovery::failed_transition(
                    graph,
                    FailureReason::ProtocolViolation(
                        "event delivered to terminal state".to_string(),
                    ),
                )
            }
        }
    }

    /// Recognise terminal states and extract the final output.
    ///
    /// Returns `Some` only for `Complete` and `Failed`, the two states from
    /// which the scheduler cannot advance further. All other states return
    /// `None` to keep the runner loop going.
    pub fn output(&self, state: &SchedulerState) -> Option<SchedulerTerminalOutput> {
        match state {
            SchedulerState::Complete { graph } => Some(SchedulerTerminalOutput::Complete {
                recovery_summary: RecoverySummary::from_graph(graph),
                graph: graph.clone(),
            }),
            SchedulerState::Failed { graph, reason } => Some(SchedulerTerminalOutput::Failed {
                graph: graph.clone(),
                reason: reason.clone(),
            }),
            _ => None,
        }
    }
}

fn node_event_id(event: &SchedulerEvent) -> &NodeId {
    match event {
        SchedulerEvent::PlanAccepted { node_id, .. }
        | SchedulerEvent::WorkAccepted { node_id, .. }
        | SchedulerEvent::NodeFailed { node_id, .. } => node_id,
        _ => unreachable!("not a node result event"),
    }
}

fn integration_event_id(event: &SchedulerEvent) -> &NodeId {
    match event {
        SchedulerEvent::IntegrationSucceeded { node_id, .. }
        | SchedulerEvent::IntegrationFailed { node_id, .. } => node_id,
        _ => unreachable!("not an integration result event"),
    }
}

#[cfg(test)]
#[path = "machine_tests/mod.rs"]
mod machine_tests;
