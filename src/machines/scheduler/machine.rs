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

/// Maximum number of attempts allowed per objective before recovery stops.
///
/// Attempts are zero-based: attempts 0, 1, 2, and 3 are all valid runs.
/// When `node.attempt >= MAX_ATTEMPTS`, `Retry`, `ElevateModel`, and `Split`
/// recovery will not create a replacement — the scheduler transitions to
/// `Failed`.
const MAX_ATTEMPTS: u32 = 3;

/// Scheduler circuit breaker for graph growth.
///
/// This is not a business limit. It exists only to bound recursive planning and
/// recovery expansion so a run cannot append nodes forever.
const MAX_GRAPH_NODES: usize = 100;
use super::event::{
    IntegrationFailure, IntegrationOutcome, IntegrationOutput, NodeFailure, NodeOutcome,
    NodeOutcome::*, NodeRequest, RecoveryAction, SchedulerEvent, WorkOutput,
};
use super::state::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest, SchedulerState,
};

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
/// `SchedulerMachine` is a zero-sized marker struct; all of its data travels
/// inside `SchedulerState`. This follows the project pattern where machines do
/// not own mutable fields — they are pure transition logic carriers.
pub struct SchedulerMachine;

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
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
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

    /// Returns `true` when no node is still `Pending`, `Running`, or `Integrating`.
    ///
    /// `Failed` and `Cancelled` nodes count as terminal for this scan. Terminal
    /// recovery transitions directly to `SchedulerState::Failed`, while
    /// recoverable failures leave failed historical nodes behind and continue
    /// through replacement nodes. `Integrating` is active and therefore blocks
    /// completion just like `Pending` and `Running`.
    fn all_complete(graph: &RunGraph) -> bool {
        !graph.nodes.iter().any(|n| {
            matches!(
                n.status,
                NodeStatus::Pending | NodeStatus::Running | NodeStatus::Integrating
            )
        })
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
                origin: NodeOrigin::PlanExpansion,
            });
        }
        graph
    }

    /// Rewrite `old_id` → `new_id` in the `dependencies` of every `Pending` node.
    ///
    /// Only `Pending` nodes are mutable; `Running`, `Completed`, `Failed`, and
    /// `Cancelled` nodes are historical records and must not be touched.
    fn remap_pending_dependencies(graph: RunGraph, old_id: &NodeId, new_id: &NodeId) -> RunGraph {
        let next_id = graph.next_id;
        RunGraph {
            nodes: graph
                .nodes
                .into_iter()
                .map(|mut n| {
                    if n.status == NodeStatus::Pending {
                        n.dependencies = n
                            .dependencies
                            .into_iter()
                            .map(|dep| if &dep == old_id { new_id.clone() } else { dep })
                            .collect();
                    }
                    n
                })
                .collect(),
            next_id,
        }
    }

    /// Verify that every dependency listed in every child request already exists
    /// in the graph.
    ///
    /// Returns `Err` with a descriptive message on the first unknown reference.
    /// The graph is not mutated; callers must not insert children when this
    /// returns `Err`.
    fn validate_plan_dependencies(
        graph: &RunGraph,
        children: &[NodeRequest],
    ) -> Result<(), String> {
        let known: HashSet<&NodeId> = graph.nodes.iter().map(|n| &n.id).collect();
        for child in children {
            for dep in &child.dependencies {
                if !known.contains(dep) {
                    return Err(format!(
                        "plan output references unknown node id: {:?}",
                        dep.0
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validate structural invariants of an externally supplied `RunGraph`.
    ///
    /// Returns `Err` with a human-readable reason on the first violation found.
    /// Called once at `Running + Start` before any scheduling logic runs.
    ///
    /// Invariants checked:
    /// 1. No two nodes share the same `NodeId`.
    /// 2. Every dependency in every node references an existing `NodeId`.
    ///
    /// `RunGraph::next_id` is an internal generator cursor. Validation treats
    /// `NodeId` values as opaque and does not parse their string form to infer
    /// whether the cursor is stale.
    fn validate_graph_invariants(graph: &RunGraph) -> Result<(), String> {
        let mut seen: HashSet<&NodeId> = HashSet::new();
        for node in &graph.nodes {
            if !seen.insert(&node.id) {
                return Err(format!("duplicate node id: {}", node.id.0));
            }
        }

        let all_ids: HashSet<&NodeId> = graph.nodes.iter().map(|n| &n.id).collect();
        for node in &graph.nodes {
            for dep in &node.dependencies {
                if !all_ids.contains(dep) {
                    return Err(format!(
                        "missing dependency: node {} depends on unknown id {}",
                        node.id.0, dep.0
                    ));
                }
            }
        }

        Ok(())
    }

    /// Classify why no node became ready when the graph is not yet complete.
    ///
    /// Checks pending nodes for missing dependencies first. If all referenced
    /// IDs exist but no node is ready, the graph is stuck in a cycle or a
    /// mutually blocked chain.
    fn diagnose_no_ready(graph: &RunGraph) -> String {
        let existing: HashSet<&NodeId> = graph.nodes.iter().map(|n| &n.id).collect();
        for node in &graph.nodes {
            if node.status == NodeStatus::Pending {
                for dep in &node.dependencies {
                    if !existing.contains(dep) {
                        return format!(
                            "pending node {} has missing dependency {}",
                            node.id.0, dep.0
                        );
                    }
                }
            }
        }
        "no ready nodes: blocked dependency chain or possible cycle".to_string()
    }

    /// Returns `true` when the node has already consumed all permitted attempts.
    ///
    /// Used by `Retry`, `ElevateModel`, and `Split` recovery arms to guard
    /// against infinite recovery loops.
    fn attempts_exhausted(node: &Node) -> bool {
        node.attempt >= MAX_ATTEMPTS
    }

    /// Return whether the graph can accept `additional_nodes` more appended nodes.
    fn graph_has_capacity(graph: &RunGraph, additional_nodes: usize) -> bool {
        graph
            .nodes
            .len()
            .checked_add(additional_nodes)
            .is_some_and(|total| total <= MAX_GRAPH_NODES)
    }

    fn graph_size_limit_reason(additional_nodes: usize) -> String {
        format!(
            "graph size limit exceeded: cannot add {additional_nodes} node(s); limit is {MAX_GRAPH_NODES}"
        )
    }

    fn failed_transition(
        graph: RunGraph,
        reason: String,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        Transition {
            state: SchedulerState::Failed {
                graph: graph.clone(),
                reason: reason.clone(),
            },
            effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
        }
    }

    /// Mark every `Pending` node that depends (directly or indirectly) on
    /// `failed_id` as `Cancelled`.
    ///
    /// The failed node itself must already be marked `Failed` before this is
    /// called. Only `Pending` nodes are eligible; all other statuses are left
    /// untouched.
    fn cancel_pending_dependents(graph: RunGraph, failed_id: &NodeId) -> RunGraph {
        let mut tainted: HashSet<NodeId> = HashSet::new();
        tainted.insert(failed_id.clone());

        loop {
            let mut grew = false;
            for node in &graph.nodes {
                if node.status == NodeStatus::Pending
                    && !tainted.contains(&node.id)
                    && node.dependencies.iter().any(|dep| tainted.contains(dep))
                {
                    tainted.insert(node.id.clone());
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }

        tainted.remove(failed_id);

        let next_id = graph.next_id;
        RunGraph {
            nodes: graph
                .nodes
                .into_iter()
                .map(|mut n| {
                    if tainted.contains(&n.id) {
                        n.status = NodeStatus::Cancelled;
                    }
                    n
                })
                .collect(),
            next_id,
        }
    }

    /// Handle a `Retry` recovery: mark the failed node and create a replacement.
    ///
    /// The replacement carries the same objective, kind, model tier, and
    /// dependencies as the original, with `attempt` incremented. The original
    /// node is marked `Failed` — it is never removed or mutated further.
    ///
    /// After inserting the replacement, all `Pending` downstream nodes that
    /// referenced `node_id` are remapped to reference `replacement_id` so that
    /// the graph does not deadlock waiting for a `Failed` node to complete.
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
            id: replacement_id.clone(),
            kind,
            objective,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            model_tier,
            summary: None,
            origin: NodeOrigin::Retry {
                source: node_id.clone(),
            },
        };
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        let graph = Self::push_node(graph, replacement);
        Self::remap_pending_dependencies(graph, node_id, &replacement_id)
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
        let (deps, attempt) = {
            let n = Self::get_node(&graph, node_id);
            (n.dependencies.clone(), n.attempt)
        };
        let split_id = NodeId(format!("{}-split-{}", node_id.0, graph.next_id));
        let split_node = Node {
            id: split_id.clone(),
            kind: NodeKind::Plan,
            objective: message,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            model_tier: ModelTier::Strong,
            summary: None,
            origin: NodeOrigin::Split {
                source: node_id.clone(),
            },
        };
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        let graph = Self::push_node(graph, split_node);
        Self::remap_pending_dependencies(graph, node_id, &split_id)
    }

    /// Route a `RecoveryAction` for a failed or failed-integration node.
    ///
    /// Both `NodeOutcome::Failed` and `IntegrationOutcome::Failed` share the
    /// same four recovery branches. This helper centralises the routing so
    /// neither caller duplicates the exhaustion checks or graph mutations.
    ///
    /// The caller is responsible for ensuring `node_id` is present in `graph`.
    fn route_recovery(
        graph: RunGraph,
        node_id: &NodeId,
        recovery: RecoveryAction,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        match recovery {
            RecoveryAction::Retry { .. } => {
                let exhausted = Self::attempts_exhausted(Self::get_node(&graph, node_id));
                if exhausted {
                    let reason = format!(
                        "node {} exhausted all {} attempts (Retry)",
                        node_id.0, MAX_ATTEMPTS
                    );
                    let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                    Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    }
                } else if !Self::graph_has_capacity(&graph, 1) {
                    let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                    Self::failed_transition(graph, Self::graph_size_limit_reason(1))
                } else {
                    let graph = Self::apply_retry(graph, node_id);
                    Transition {
                        state: SchedulerState::Running { graph },
                        effects: vec![],
                    }
                }
            }

            RecoveryAction::Split { message } => {
                let exhausted = Self::attempts_exhausted(Self::get_node(&graph, node_id));
                if exhausted {
                    let reason = format!(
                        "node {} exhausted all {} attempts (Split)",
                        node_id.0, MAX_ATTEMPTS
                    );
                    let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                    Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    }
                } else if !Self::graph_has_capacity(&graph, 1) {
                    let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                    Self::failed_transition(graph, Self::graph_size_limit_reason(1))
                } else {
                    let graph = Self::apply_split(graph, node_id, message);
                    Transition {
                        state: SchedulerState::Running { graph },
                        effects: vec![],
                    }
                }
            }

            RecoveryAction::ElevateModel { .. } => {
                let exhausted = Self::attempts_exhausted(Self::get_node(&graph, node_id));
                if exhausted {
                    let reason = format!(
                        "node {} exhausted all {} attempts (ElevateModel)",
                        node_id.0, MAX_ATTEMPTS
                    );
                    let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                    Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    }
                } else if !Self::graph_has_capacity(&graph, 1) {
                    let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                    Self::failed_transition(graph, Self::graph_size_limit_reason(1))
                } else {
                    let graph = Self::apply_elevate(graph, node_id);
                    Transition {
                        state: SchedulerState::Running { graph },
                        effects: vec![],
                    }
                }
            }

            RecoveryAction::Terminal { message } => {
                let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
                let graph = Self::cancel_pending_dependents(graph, node_id);
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
        }
    }

    /// Handle an `ElevateModel` recovery: create a replacement at `Strong` tier.
    ///
    /// Preserves the exact same objective so the stronger model retries the same
    /// goal. Unlike `Retry`, the model tier is unconditionally upgraded to
    /// `Strong`, because the failure signal indicates the task is beyond
    /// `Cheap` tier capacity.
    ///
    /// After inserting the replacement, all `Pending` downstream nodes that
    /// referenced `node_id` are remapped to reference `replacement_id` so that
    /// the graph does not deadlock waiting for a `Failed` node to complete.
    /// Find a node by ID without panicking.
    ///
    /// Used for validation where a missing node means an invalid event, not an
    /// internal inconsistency.
    fn node_for_running<'a>(graph: &'a RunGraph, node_id: &NodeId) -> Option<&'a Node> {
        graph.nodes.iter().find(|n| &n.id == node_id)
    }

    /// Return `Some(reason)` when `outcome` is incompatible with `node_kind`.
    ///
    /// `Failed` is always valid. `PlanAccepted` is only valid for Plan nodes;
    /// `WorkAccepted` is only valid for Work nodes.
    fn invalid_node_outcome_reason(
        node_id: &NodeId,
        node_kind: &NodeKind,
        outcome: &NodeOutcome,
    ) -> Option<String> {
        match (node_kind, outcome) {
            (NodeKind::Work, PlanAccepted(_)) => Some(format!(
                "node {} is Work but received PlanAccepted outcome",
                node_id.0
            )),
            (NodeKind::Plan, WorkAccepted(_)) => Some(format!(
                "node {} is Plan but received WorkAccepted outcome",
                node_id.0
            )),
            _ => None,
        }
    }

    /// Return `Some(reason)` when `IntegrationReturned` is not valid for the node.
    ///
    /// Valid only when the node exists, has `NodeKind::Work`, and is in
    /// `NodeStatus::Integrating`.
    fn invalid_integration_reason(graph: &RunGraph, node_id: &NodeId) -> Option<String> {
        match Self::node_for_running(graph, node_id) {
            None => Some(format!("node {} not found in graph", node_id.0)),
            Some(node) if node.kind != NodeKind::Work => Some(format!(
                "node {} is {:?} but IntegrationReturned requires a Work node",
                node_id.0, node.kind
            )),
            Some(node) if node.status != NodeStatus::Integrating => Some(format!(
                "node {} has status {:?} but IntegrationReturned requires Integrating",
                node_id.0, node.status
            )),
            _ => None,
        }
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
            id: elevated_id.clone(),
            kind,
            objective,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            model_tier: ModelTier::Strong,
            summary: None,
            origin: NodeOrigin::ElevateModel {
                source: node_id.clone(),
            },
        };
        let graph = Self::mark_node(graph, node_id, NodeStatus::Failed);
        let graph = Self::push_node(graph, replacement);
        Self::remap_pending_dependencies(graph, node_id, &elevated_id)
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
                if let Err(reason) = Self::validate_graph_invariants(&graph) {
                    return Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    };
                }
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
                        let reason = Self::diagnose_no_ready(&graph);
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

                // Validate that the outcome is compatible with the node's kind.
                let node_kind = Self::get_node(&graph, &node_id).kind.clone();
                if let Some(reason) =
                    Self::invalid_node_outcome_reason(&node_id, &node_kind, &outcome)
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
                    // leaves the graph entirely unchanged and fails the run immediately.
                    PlanAccepted(plan) => {
                        match Self::validate_plan_dependencies(&graph, &plan.children) {
                            Err(reason) => Transition {
                                state: SchedulerState::Failed {
                                    graph: graph.clone(),
                                    reason: reason.clone(),
                                },
                                effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                            },
                            Ok(()) if !Self::graph_has_capacity(&graph, plan.children.len()) => {
                                let reason = Self::graph_size_limit_reason(plan.children.len());
                                Transition {
                                    state: SchedulerState::Failed {
                                        graph: graph.clone(),
                                        reason: reason.clone(),
                                    },
                                    effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                                }
                            }
                            Ok(()) => {
                                let graph = Self::mark_node(graph, &node_id, NodeStatus::Completed);
                                let graph = Self::insert_children(graph, &node_id, plan.children);
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
                        let graph = Self::mark_node(graph, &node_id, NodeStatus::Integrating);
                        Transition {
                            state: SchedulerState::Waiting {
                                graph,
                                running: node_id.clone(),
                            },
                            effects: vec![SchedulerEffect::IntegrateWork { node_id, work }],
                        }
                    }

                    Failed(NodeFailure {
                        reason: _,
                        recovery,
                    }) => Self::route_recovery(graph, &node_id, recovery),
                }
            }

            // Integration finished: success marks the node Completed and
            // resumes scanning; failure routes through the same recovery
            // machinery as execution failure.
            (
                SchedulerState::Waiting { graph, running },
                SchedulerEvent::IntegrationReturned { node_id, outcome },
            ) => {
                assert_eq!(
                    running, node_id,
                    "returned integration does not match running node"
                );

                // Validate that integration arrives for a Work node in Integrating status.
                if let Some(reason) = Self::invalid_integration_reason(&graph, &node_id) {
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
                        let graph = Self::mark_node_completed_with_summary(
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
                        reason: _,
                        recovery,
                    }) => Self::route_recovery(graph, &node_id, recovery),
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

            SchedulerEffect::IntegrateWork { node_id, work } => {
                println!(
                    "  -> integrating work from node {}: {:?}",
                    node_id.0, work.summary
                );
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

    /// Recognise terminal states and extract the final output.
    ///
    /// Returns `Some` only for `Complete` and `Failed`, the two states from
    /// which the scheduler cannot advance further. All other states return
    /// `None` to keep the runner loop going.
    fn output(&self, state: &Self::State) -> Option<Self::Output> {
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
    use crate::machines::scheduler::event::{
        IntegrationFailure, IntegrationOutcome, IntegrationOutput, NodeFailure, NodeOutcome,
        NodeRequest, PlanOutput, RecoveryAction, WorkOutput,
    };
    use crate::machines::scheduler::state::{Node, RunGraph, RunRequest};

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
            origin: NodeOrigin::Root,
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
        SchedulerMachine.transition(state, event)
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
        let output = crate::engine::run_machine(SchedulerMachine, state);
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
                origin: NodeOrigin::Root,
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
    fn retry_remaps_downstream_dependencies_and_chain_completes() {
        // A -> B -> C; B fails with Retry on attempt 0, succeeds on attempt 1.
        // After Retry the stub handler returns WorkAccepted because "do retry"
        // contains "retry" and attempt > 0.
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "do retry", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };

        let output =
            crate::engine::run_machine(SchedulerMachine, SchedulerState::Running { graph });

        let SchedulerOutput::Complete { graph, .. } = output else {
            panic!("expected Complete")
        };

        // Original B is Failed (historical record).
        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        // Replacement B' exists and completed.
        let b_prime = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("B-retry-"))
            .expect("B'");
        assert_eq!(b_prime.status, NodeStatus::Completed);
        assert_eq!(b_prime.attempt, 1);

        // C's dependency was rewritten from B to B'.
        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&b_prime.id),
            "C does not depend on B'"
        );

        // C ran and completed.
        assert_eq!(c.status, NodeStatus::Completed);
    }

    #[test]
    fn elevate_remaps_downstream_dependencies_and_chain_completes() {
        // A -> B -> C; B fails with ElevateModel on attempt 0, succeeds on attempt 1.
        // The stub handler returns ElevateModel on attempt 0 and WorkAccepted on attempt 1
        // because "do elevate" contains "elevate".
        let graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "do elevate", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
        };

        let output =
            crate::engine::run_machine(SchedulerMachine, SchedulerState::Running { graph });

        let SchedulerOutput::Complete { graph, .. } = output else {
            panic!("expected Complete, got Failed")
        };

        // Original B is Failed (historical record).
        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        // Replacement B' exists, used Strong tier, and completed.
        let b_prime = graph
            .nodes
            .iter()
            .find(|n| n.id.0.starts_with("B-elevated-"))
            .expect("B'");
        assert_eq!(b_prime.model_tier, ModelTier::Strong);
        assert_eq!(b_prime.attempt, 1);
        assert_eq!(b_prime.status, NodeStatus::Completed);

        // C's dependency was rewritten from B to B'.
        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&b_prime.id),
            "C does not depend on B'"
        );

        // C ran and completed.
        assert_eq!(c.status, NodeStatus::Completed);
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
                    reason: "task too complex".to_string(),
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
        let output = crate::engine::run_machine(
            SchedulerMachine,
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
    fn retry_exhaustion_fails_scheduler() {
        // Verify that a Retry recovery on a node already at MAX_ATTEMPTS:
        //   1. does not insert a replacement node,
        //   2. marks the original node Failed,
        //   3. transitions the scheduler to SchedulerState::Failed.
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
                    reason: "transient error".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "try again".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        // No replacement node was created.
        assert_eq!(
            graph.nodes.len(),
            1,
            "no replacement node should be created"
        );
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            reason.contains("exhausted"),
            "reason should mention exhaustion, got: {reason:?}"
        );
        // The ReturnFailed effect was emitted.
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
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
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
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
                        kind: NodeKind::Work,
                        objective: "child work".to_string(),
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
                            kind: NodeKind::Work,
                            objective: "child one".to_string(),
                            dependencies: vec![NodeId("P".to_string())],
                        },
                        NodeRequest {
                            kind: NodeKind::Work,
                            objective: "child two".to_string(),
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

    #[test]
    fn elevate_exhaustion_fails_scheduler() {
        // Verify that an ElevateModel recovery on a node already at MAX_ATTEMPTS:
        //   1. does not insert a replacement node,
        //   2. marks the original node Failed,
        //   3. transitions the scheduler to SchedulerState::Failed.
        let mut node = work_node("W", "hard task", &[]);
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
                    reason: "capability ceiling".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "escalate model".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        // No replacement node was created.
        assert_eq!(
            graph.nodes.len(),
            1,
            "no replacement node should be created"
        );
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            reason.contains("exhausted"),
            "reason should mention exhaustion, got: {reason:?}"
        );
        // The ReturnFailed effect was emitted.
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
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
                    reason: "unrecoverable".to_string(),
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

    // ── Split attempt-limit tests ─────────────────────────────────────────────

    #[test]
    fn split_exhaustion_fails_scheduler() {
        // A node already at MAX_ATTEMPTS that returns Split must not create a
        // replacement Plan node; the scheduler transitions to Failed immediately.
        let mut node = work_node("W", "complex task", &[]);
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
                    reason: "task too complex".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose the work".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(
            graph.nodes.len(),
            1,
            "no split replacement node should be created"
        );
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            reason.contains("exhausted"),
            "reason should mention exhaustion, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn retry_respects_graph_size_limit() {
        let graph = graph_with_filler_nodes(work_node("W", "failing task", &[]), MAX_GRAPH_NODES);

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    reason: "transient error".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "try again".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES);
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| !matches!(node.origin, NodeOrigin::Retry { .. })),
            "no retry replacement should be created"
        );
        assert!(reason.contains("graph size limit"));
        assert!(reason.contains(&MAX_GRAPH_NODES.to_string()));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn split_respects_graph_size_limit() {
        let graph = graph_with_filler_nodes(work_node("W", "complex task", &[]), MAX_GRAPH_NODES);

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    reason: "task too complex".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose the work".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES);
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| !matches!(node.origin, NodeOrigin::Split { .. })),
            "no split replacement should be created"
        );
        assert!(reason.contains("graph size limit"));
        assert!(reason.contains(&MAX_GRAPH_NODES.to_string()));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn elevate_respects_graph_size_limit() {
        let graph = graph_with_filler_nodes(work_node("W", "hard task", &[]), MAX_GRAPH_NODES);

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    reason: "capability ceiling".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use stronger model".to_string(),
                    },
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("expected Failed, got {:#?}", t.state);
        };
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES);
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| !matches!(node.origin, NodeOrigin::ElevateModel { .. })),
            "no elevate replacement should be created"
        );
        assert!(reason.contains("graph size limit"));
        assert!(reason.contains(&MAX_GRAPH_NODES.to_string()));
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    // ── RecoverySummary / output classification tests ─────────────────────────

    #[test]
    fn clean_success_has_no_recovery() {
        let output = crate::engine::run_machine(
            SchedulerMachine,
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
    fn retry_success_reports_recovery() {
        // The stub handler triggers Retry on attempt 0 and succeeds on attempt 1
        // for any objective containing "retry".
        let output = crate::engine::run_machine(
            SchedulerMachine,
            SchedulerState::Running {
                graph: RunGraph {
                    nodes: vec![work_node("R", "retry this", &[])],
                    next_id: 0,
                },
            },
        );
        let SchedulerOutput::Complete {
            recovery_summary, ..
        } = output
        else {
            panic!("expected Complete");
        };
        assert!(recovery_summary.recovered);
        assert_eq!(recovery_summary.retry_count, 1);
        assert_eq!(recovery_summary.elevate_count, 0);
        assert_eq!(recovery_summary.split_count, 0);
    }

    #[test]
    fn elevate_success_reports_recovery() {
        // The stub handler triggers ElevateModel on attempt 0 and succeeds on
        // attempt 1 for any objective containing "elevate".
        let output = crate::engine::run_machine(
            SchedulerMachine,
            SchedulerState::Running {
                graph: RunGraph {
                    nodes: vec![work_node("E", "elevate this", &[])],
                    next_id: 0,
                },
            },
        );
        let SchedulerOutput::Complete {
            recovery_summary, ..
        } = output
        else {
            panic!("expected Complete");
        };
        assert!(recovery_summary.recovered);
        assert_eq!(recovery_summary.retry_count, 0);
        assert_eq!(recovery_summary.elevate_count, 1);
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
                    dependencies: vec![],
                    status: NodeStatus::Failed,
                    attempt: 0,
                    model_tier: ModelTier::Cheap,
                    summary: None,
                    origin: NodeOrigin::Root,
                },
                Node {
                    id: split_id,
                    kind: NodeKind::Plan,
                    objective: "decompose complex task".to_string(),
                    dependencies: vec![],
                    status: NodeStatus::Completed,
                    attempt: 1,
                    model_tier: ModelTier::Strong,
                    summary: Some("planned".to_string()),
                    origin: NodeOrigin::Split { source: source_id },
                },
            ],
            next_id: 1,
        };
        let state = SchedulerState::Complete { graph };
        let output = SchedulerMachine
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
                    reason: "task too complex".to_string(),
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
                    reason: "integration error".to_string(),
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
                    reason: "integration error".to_string(),
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
                    reason: "integration error".to_string(),
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
                    reason: "integration error".to_string(),
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
    fn integration_failure_retry_exhaustion_fails_scheduler() {
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
                    reason: "integration error".to_string(),
                    recovery: RecoveryAction::Retry {
                        message: "retry integration".to_string(),
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
            reason.contains("exhausted"),
            "reason should mention exhaustion, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn integration_failure_elevate_exhaustion_fails_scheduler() {
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
                    reason: "integration error".to_string(),
                    recovery: RecoveryAction::ElevateModel {
                        message: "use stronger model".to_string(),
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
            reason.contains("exhausted"),
            "reason should mention exhaustion, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
    }

    #[test]
    fn integration_failure_split_exhaustion_fails_scheduler() {
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
                    reason: "integration error".to_string(),
                    recovery: RecoveryAction::Split {
                        message: "decompose step B".to_string(),
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
            reason.contains("exhausted"),
            "reason should mention exhaustion, got: {reason:?}"
        );
        assert!(matches!(
            t.effects.as_slice(),
            [SchedulerEffect::ReturnFailed { .. }]
        ));
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
                    reason: "unrecoverable".to_string(),
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
