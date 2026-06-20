//! Scheduler machine — transition logic and graph helpers.
//!
//! This module owns the `SchedulerMachine` implementation of `Machine`. It
//! contains:
//!
//! - Pure graph-inspection helpers (`find_ready`, `all_complete`).
//! - Pure graph-mutation helpers (`mark_node`, `push_node`, `insert_children`,
//!   `apply_retry`, `apply_split`, `apply_elevate`).
//! - The `transition` function, which is the only place where state advances.
//! - The `handle_effect` function, which simulates runners during development.
//! - An `output` recogniser that identifies terminal states.
//!
//! # What this module does NOT own
//!
//! - The definitions of state, events, and effects — those live in their
//!   respective sibling modules.
//! - The generic runner loop — that is in `engine::runner`.
//! - Real provider or tool execution — this stub dispatches keyword-based
//!   outcomes for demonstration purposes only.

use std::collections::HashSet;

use crate::engine::{Machine, Transition};

use super::effect::SchedulerEffect;
use super::event::{
    NodeFailure, NodeOutcome, NodeOutcome::*, NodeRequest, RecoveryAction, SchedulerEvent,
    WorkOutput,
};
use super::state::{ModelTier, Node, NodeId, NodeKind, NodeStatus, RunGraph, SchedulerState};

/// The terminal result of a complete scheduler run.
///
/// The caller (`run_machine` or `RunMachine`) receives this when the scheduler
/// reaches either of its two terminal states.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerOutput {
    /// Every node in the graph reached `Completed`. The final graph is returned
    /// for audit and to extract work summaries.
    Complete(RunGraph),
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
/// `SchedulerMachine` is a zero-sized marker struct; all of its data travels
/// inside `SchedulerState`. This follows the project pattern where machines do
/// not own mutable fields — they are pure transition logic carriers.
pub struct SchedulerMachine;

impl SchedulerMachine {
    /// Returns the IDs of all nodes that are `Pending` and whose every
    /// dependency has reached `Completed`.
    ///
    /// A node is eligible to run only when its full dependency set is satisfied.
    /// `Running`, `Failed`, and `Cancelled` dependencies do *not* satisfy the
    /// check — only `Completed` does.
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

    /// Returns `true` when no node is still `Pending` or `Running`.
    ///
    /// `Failed` and `Cancelled` nodes count as "done" for this purpose because
    /// terminal failures exit immediately via `SchedulerState::Failed` before
    /// this check is reached. A graph where some nodes are `Failed` but none
    /// are `Pending` or `Running` means recovery created no further work —
    /// which is only possible if the graph has genuinely finished.
    ///
    /// TODO: track a separate "active leaf count" when cancellation propagation
    /// is added, so that `Cancelled` nodes are handled distinctly.
    fn all_complete(graph: &RunGraph) -> bool {
        !graph
            .nodes
            .iter()
            .any(|n| matches!(n.status, NodeStatus::Pending | NodeStatus::Running))
    }

    /// Update a single node's status, leaving all other nodes unchanged.
    ///
    /// The entire `nodes` vec is rebuilt via iterator to keep ownership simple.
    /// This is intentionally not `O(1)`, but graphs are small enough that the
    /// difference is irrelevant at this stage.
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

    /// Mark a node `Completed` and attach the work summary in one pass.
    ///
    /// Doing both mutations together avoids a second vec scan that `mark_node`
    /// would require if called separately.
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

    /// Look up a node by ID, panicking if it is absent.
    ///
    /// Every node that the scheduler dispatches is present in the graph by
    /// construction. A missing node_id here indicates a bug in the event
    /// routing, not a recoverable runtime condition.
    fn get_node<'a>(graph: &'a RunGraph, node_id: &NodeId) -> &'a Node {
        graph
            .nodes
            .iter()
            .find(|n| &n.id == node_id)
            .expect("node not found in graph")
    }

    /// Append a node to the graph and advance the ID counter.
    fn push_node(mut graph: RunGraph, node: Node) -> RunGraph {
        graph.nodes.push(node);
        graph.next_id += 1;
        graph
    }

    /// Insert children produced by a plan node into the graph.
    ///
    /// Each `NodeRequest` becomes a real node with a fresh ID derived from the
    /// parent's ID and the current counter. Children always start at `attempt 0`
    /// with `ModelTier::Cheap`; the planner specifies everything else.
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

    /// Handle a `Retry` recovery: mark the failed node and create a replacement.
    ///
    /// The replacement carries the same objective, kind, model tier, and
    /// dependencies as the original, with `attempt` incremented. The original
    /// node is marked `Failed` — it is never removed or mutated further.
    ///
    /// TODO: downstream nodes that depend on `node_id` will stall because
    /// `node_id` is now `Failed`, not `Completed`. Fixing this requires either
    /// remapping dependencies to point at the replacement, or making
    /// `find_ready` aware of retry chains.
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
    }

    /// Handle a `Split` recovery: mark the failed node and insert a plan node.
    ///
    /// The new plan node is always `ModelTier::Strong`, regardless of the tier
    /// the original node used. Planning quality directly determines how much
    /// downstream work is created, so maximum capability is warranted here even
    /// if it is expensive.
    ///
    /// The original node is marked `Failed` (not `Cancelled`) so the audit trail
    /// is unambiguous: it attempted its objective and could not complete it.
    fn apply_split(graph: RunGraph, node_id: &NodeId, message: String) -> RunGraph {
        let (deps, model_tier) = {
            let n = Self::get_node(&graph, node_id);
            (n.dependencies.clone(), n.model_tier.clone())
        };
        let _ = model_tier; // Split always uses Strong to maximize plan quality.
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
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        Self::push_node(graph, split_node)
    }

    /// Handle an `ElevateModel` recovery: create a replacement at `Strong` tier.
    ///
    /// Preserves the exact same objective so the stronger model retries the same
    /// goal. Unlike `Retry`, the model tier is unconditionally upgraded to
    /// `Strong`, because the failure signal indicates the task is beyond
    /// `Cheap` tier capacity.
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
            // Scan the graph, then in the same tick either complete, fail, or dispatch.
            //
            // Three outcomes:
            //   1. All nodes are terminal → emit ReturnComplete and stop.
            //   2. Some nodes are Pending but none are ready → deadlock; emit ReturnFailed.
            //   3. At least one node is ready → mark it Running, emit RunNode, move to Waiting.
            (SchedulerState::Running { graph }, SchedulerEvent::Start) => {
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
                assert_eq!(
                    running, node_id,
                    "returned node does not match running node"
                );

                match outcome {
                    // A successful planner expands the graph: the plan node is marked
                    // Completed and its requested children are inserted as new Pending
                    // nodes. The scheduler then re-scans for ready nodes.
                    PlanAccepted(plan) => {
                        let graph = Self::mark_node(graph, &node_id, NodeStatus::Completed);
                        let graph = Self::insert_children(graph, &node_id, plan.children);
                        Transition {
                            state: SchedulerState::Running { graph },
                            effects: vec![],
                        }
                    }

                    // A successful worker is marked Completed with its summary attached.
                    // The summary is kept in the graph as an audit record and may serve
                    // as context for downstream nodes.
                    WorkAccepted(work) => {
                        let graph =
                            Self::mark_node_completed_with_summary(graph, &node_id, work.summary);
                        Transition {
                            state: SchedulerState::Running { graph },
                            effects: vec![],
                        }
                    }

                    Failed(NodeFailure {
                        reason: _,
                        recovery,
                    }) => match recovery {
                        // Retry: the same objective is worth attempting again as-is.
                        // A replacement node with the same tier is inserted;
                        // the original node remains in the graph as Failed.
                        RecoveryAction::Retry { .. } => {
                            let graph = Self::apply_retry(graph, &node_id);
                            Transition {
                                state: SchedulerState::Running { graph },
                                effects: vec![],
                            }
                        }

                        // Split: the task was too large or ambiguous to execute directly.
                        // A new Plan node at Strong tier is inserted to decompose it.
                        // The original node is marked Failed; the plan result will create
                        // the actual replacement work.
                        RecoveryAction::Split { message } => {
                            let graph = Self::apply_split(graph, &node_id, message);
                            Transition {
                                state: SchedulerState::Running { graph },
                                effects: vec![],
                            }
                        }

                        // Model escalation: the same objective is retried at Strong tier.
                        // The original node is marked Failed; model escalation preserves
                        // the objective exactly — only the capability level changes.
                        RecoveryAction::ElevateModel { .. } => {
                            let graph = Self::apply_elevate(graph, &node_id);
                            Transition {
                                state: SchedulerState::Running { graph },
                                effects: vec![],
                            }
                        }

                        // Terminal failure: no recovery is possible. The run stops
                        // immediately. The graph is preserved with the node marked Failed
                        // so callers can inspect what completed before the failure.
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

    /// Stub effect handler used during development.
    ///
    /// In production this would be replaced by a real `RunMachine` handler that
    /// dispatches nodes to actual LLM providers. For now, outcomes are determined
    /// by keyword matching on the objective string so that the scenarios in
    /// `main.rs` can exercise all recovery paths without a provider.
    ///
    /// `ReturnComplete` and `ReturnFailed` effects must never reach this method;
    /// the `output` recogniser intercepts terminal states before the runner has
    /// a chance to dispatch their effects.
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

    /// Recognise terminal states and extract the final output.
    ///
    /// Returns `Some` only for `Complete` and `Failed`, the two states from
    /// which the scheduler cannot advance further. All other states return
    /// `None` to keep the runner loop going.
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
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
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

        let SchedulerState::Running { graph } = t.state else {
            panic!("expected Running")
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
            crate::engine::run_machine(SchedulerMachine, SchedulerState::Running { graph });
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
            SchedulerState::Running {
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
