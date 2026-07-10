//! Graph and node types plus pure graph helpers for `SchedulerMachine`.
//!
//! This module owns the durable data shapes the scheduler carries between
//! transitions (all node descriptors and the run graph) plus the pure graph
//! inspection and mutation helpers. It does **not** own events or effects.
//!
//! Most graph inspection and mutation logic lives on `RunGraph` itself as
//! methods; a handful of depth/size helpers that don't operate on a `RunGraph`
//! remain free functions.
//!
//! # Key invariants
//!
//! - `NodeId` values are unique within a `RunGraph` and never reused.
//! - Nodes are never removed from the graph; status fields move forward only.
//! - Fresh `NodeId`s are minted with `Uuid::new_v4()` at each node creation
//!   site, so `SchedulerMachine::transition` is not a pure function of
//!   `(state, event)` for the exact ids it produces, though the resulting
//!   graph shape is otherwise deterministic.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::SchedulerEvent;
use crate::validation::ValidationPlan;

/// An opaque, stable identifier for a node in the run graph.
///
/// IDs are unique within a run and formatted as a UUID. The string form must
/// not be parsed; its internal structure is an implementation detail.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl NodeId {
    /// The first 8 characters, for compact display in logs, progress
    /// messages, and the trace viewer. Equality, dependency edges, and
    /// lookups must always use the full id; this is a display-only helper.
    pub fn short(&self) -> &str {
        self.0.get(..8).unwrap_or(&self.0)
    }
}

/// Whether a node performs planning or execution.
///
/// The distinction determines what output the scheduler expects back and how it
/// reacts to that output:
///
/// - `Plan` nodes are expected to decompose work and return child
///   [`NodeRequest`](super::types::NodeRequest)s. When accepted, the scheduler
///   inserts the requested children and continues graph traversal.
/// - `Work` nodes are expected to perform a concrete task and return a summary
///   string. When the runner reports `WorkAccepted`, the node moves to
///   `Integrating` and an `IntegrateWork` effect is emitted. The node reaches
///   `Completed` only after `IntegrationSucceeded` arrives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// A planning node. Decomposes an objective into child nodes and
    /// assigns worker roles and concrete file operations to each task.
    Plan,
    /// An execution node. Carries out a concrete, bounded task.
    Work,
}

/// Structured test-target context for a work node.
///
/// `required_validation_targets` is the adapter-derived contract attached to source
/// nodes. `planned_test_targets` is computed from graph dependency metadata at
/// dispatch time and tells reviewers whether tests are scheduled separately.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestPlanContext {
    /// Test targets required for the node's own structured target files.
    pub required_validation_targets: Vec<String>,
    /// Targets scheduled in nodes that depend on this node, directly or transitively.
    pub planned_test_targets: Vec<String>,
}

/// The model capability level to use when running a node.
///
/// `Cheap` is used for most work because cost compounds quickly across many
/// nodes. `Strong` is reserved for cases where the task has already proven too
/// difficult for the cheaper tier, or where plan quality directly determines
/// downstream work.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModelTier {
    /// The default, cost-efficient tier. Used for initial attempts.
    Cheap,
    /// The high-capability tier. Used for model-escalation retries and split
    /// recovery planning nodes.
    Strong,
}

/// How a node was introduced into the run graph.
///
/// Stored on every node so the scheduler can derive a typed `RecoverySummary`
/// from the final graph without inspecting IDs or objective strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeOrigin {
    /// The root plan node created directly from a `RunRequest`.
    Root,
    /// A node inserted by a plan node's `PlanOutput` during graph expansion.
    PlanExpansion,
    /// A replacement node created by `Retry` recovery.
    Retry {
        /// The node that failed and triggered this replacement.
        source: NodeId,
    },
    /// A replacement node created by `ElevateModel` recovery.
    ElevateModel {
        /// The node that failed and triggered this replacement.
        source: NodeId,
    },
    /// A replacement `Plan` node created by `Split` recovery.
    Split {
        /// The node that failed and triggered this plan node.
        source: NodeId,
    },
}

/// The lifecycle position of a node within the run graph.
///
/// Status only moves forward; no transition goes backward. Terminals
/// (`Completed`, `Failed`, `Cancelled`) are permanent.
///
/// # Invariant: failed nodes are historical records
///
/// A `Failed` node is never resurrected. Recovery always creates a *new*
/// replacement node, so the original failure is preserved for inspection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Not yet eligible to run; waiting for dependencies to complete.
    Pending,
    /// Dispatched to a runner; awaiting a node completion event.
    Running,
    /// Work has been produced by the runner but is not dependency-satisfying
    /// until integration succeeds.
    Integrating,
    /// Finished successfully. Dependencies on this node are now satisfiable.
    Completed,
    /// Finished unsuccessfully. The node is a permanent historical record.
    /// Recovery creates a replacement node rather than mutating this one.
    Failed,
    /// Skipped due to an upstream terminal failure. Set by the scheduler on
    /// every `Pending` node that depends (directly or transitively) on a node
    /// that received a `Terminal` recovery action.
    Cancelled,
}

/// Structured diagnostic feedback attached to a node that failed validation
/// and will be retried.
///
/// The machine stores this on the retry node instead of appending it to the
/// objective string. The dispatch layer renders it into the prompt at
/// dispatch time so the objective remains the original task description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetryFeedback {
    /// Concise diagnostic output from the failed validation attempt.
    ///
    /// Truncated by the machine to a reasonable prompt length. The format
    /// (command, exit code, diagnostic lines, etc.) is determined by the
    /// integration layer and not parsed here.
    pub diagnostics: String,
}

/// A single unit of work in the run graph.
///
/// Each node carries everything the scheduler and runner need to dispatch,
/// track, and audit it. Fields are set at creation and updated only through
/// the explicit graph-mutation helpers on `SchedulerMachine`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identifier, unique within the run graph.
    pub id: NodeId,
    /// Whether this node plans or executes.
    pub kind: NodeKind,
    /// The team that owns this node.
    ///
    /// Empty for the single-team root/plan-expansion path, where no team
    /// dispatch exists yet. Non-empty only for nodes spawned directly by
    /// team trigger evaluation, which tags them with the configured team
    /// name so later trigger evaluations can find them.
    #[serde(default)]
    pub team: String,
    /// The manifest task id this node was spawned to act on, if any.
    ///
    /// `Some` only for `AfterEach`-triggered nodes, carrying the completed
    /// task id they were spawned for. Used to detect a node already exists
    /// for a given (team, task) pair without re-reading the manifest.
    #[serde(default)]
    pub task_id: Option<String>,
    /// The adapter-assigned worker role for a `Work` node (e.g. `"tester"`).
    ///
    /// Assigned deterministically by the planner from the node's target files
    /// using the project adapter's worker definitions. `None` for `Plan`
    /// nodes and for `Work` nodes with no distinct role (the default,
    /// full-validation worker). Preserved unchanged through retries and model
    /// escalation; reset to `None` on `Split`, which always creates a `Plan`
    /// node.
    #[serde(default)]
    pub worker_role: Option<String>,
    /// A natural-language description of what this node should accomplish.
    /// Passed verbatim to the runner; preserved across retries and escalations.
    pub objective: String,
    /// Structured target files this node is expected and allowed to touch.
    ///
    /// Prompt text may render these for the model, but tooling must use this
    /// metadata instead of parsing the objective.
    pub target_files: Vec<String>,
    /// Adapter-derived test targets required for this node's target files.
    ///
    /// This is structured planner/adapter metadata. It is not inferred from
    /// objective text and is preserved across retries and model escalation.
    #[serde(default)]
    pub required_validation_targets: Vec<String>,
    /// Nodes that must be `Completed` before this node is eligible to run.
    /// The scheduler will not dispatch a node until all listed dependencies are
    /// in the `Completed` state.
    pub dependencies: Vec<NodeId>,
    /// Current lifecycle position in the graph.
    pub status: NodeStatus,
    /// Zero-based retry count. Incremented each time a replacement node is
    /// created for this objective, giving the runner observability into how
    /// many previous attempts have been made.
    pub attempt: u32,
    /// Scheduler circuit breaker metadata for recursive planning.
    ///
    /// This is not a business rule. It records ancestry through `Plan` nodes
    /// so the scheduler can bound plan chains without traversing the graph.
    /// Work nodes inherit their parent's depth without increasing it.
    pub plan_depth: usize,
    /// The model capability level to use when running this node.
    pub model_tier: ModelTier,
    /// A brief human-readable description of the outcome, set when the node
    /// reaches `Completed`. `None` until then.
    pub summary: Option<String>,
    /// How this node was introduced into the graph.
    ///
    /// Used by `RecoverySummary::from_graph` to classify a completed run
    /// without inspecting IDs or objective strings.
    pub origin: NodeOrigin,
    /// The validation contract for this node.
    ///
    /// Present only on `Work` nodes.  Set at plan-expansion time and preserved
    /// unchanged through retries, model escalations, and checkpoint/resume.
    /// When `None`, integration falls back to the handler-level validator.
    #[serde(default)]
    pub validation_plan: Option<ValidationPlan>,
    /// Structured diagnostic feedback from a failed validation attempt.
    ///
    /// Set by `apply_retry` when the failure kind is `ValidationFailure` or
    /// `WorkSemanticValidationFailure`. The objective string is left unchanged;
    /// the dispatch layer renders this into the prompt at dispatch time.
    /// `None` for the first attempt and for non-validation retries.
    #[serde(default)]
    pub retry_feedback: Option<RetryFeedback>,
}

/// The complete set of nodes for one Forge run.
///
/// The graph only grows: nodes are appended on plan expansion and recovery, but
/// never removed. This ensures the full execution history is always available
/// for debugging and audit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunGraph {
    /// All nodes, in insertion order. The ordering has no semantic meaning;
    /// the scheduler scans the vec when computing ready sets.
    pub nodes: Vec<Node>,
}

/// Maximum number of attempts allowed per objective before recovery stops.
pub(super) const MAX_ATTEMPTS: u32 = 3;

/// Scheduler circuit breaker for graph growth.
pub(super) const MAX_GRAPH_NODES: usize = 100;

/// Scheduler circuit breaker for recursive planning depth.
pub(super) const MAX_PLAN_DEPTH: usize = 10;

// ── graph queries ──────────────────────────────────────────────────────────────

impl RunGraph {
    pub(super) fn find_ready(&self) -> Vec<NodeId> {
        let completed: HashSet<&NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .map(|n| &n.id)
            .collect();

        self.nodes
            .iter()
            .filter(|n| {
                n.status == NodeStatus::Pending
                    && n.dependencies.iter().all(|dep| completed.contains(dep))
            })
            .map(|n| n.id.clone())
            .collect()
    }

    pub(super) fn all_complete(&self) -> bool {
        !self.nodes.iter().any(|n| {
            matches!(
                n.status,
                NodeStatus::Pending | NodeStatus::Running | NodeStatus::Integrating
            )
        })
    }

    pub(super) fn get_node(&self, node_id: &NodeId) -> &Node {
        self.nodes
            .iter()
            .find(|n| &n.id == node_id)
            .expect("node not found in graph")
    }

    pub(super) fn node_for_running(&self, node_id: &NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| &n.id == node_id)
    }

    pub(super) fn active_nodes(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| matches!(n.status, NodeStatus::Running | NodeStatus::Integrating))
            .collect()
    }

    pub(super) fn graph_has_capacity(&self, additional_nodes: usize) -> bool {
        self.nodes
            .len()
            .checked_add(additional_nodes)
            .is_some_and(|total| total <= MAX_GRAPH_NODES)
    }

    pub(super) fn test_plan_context_for_node(&self, node_id: &NodeId) -> TestPlanContext {
        let node = self.get_node(node_id);
        TestPlanContext {
            required_validation_targets: node.required_validation_targets.clone(),
            planned_test_targets: self.downstream_target_files(node_id),
        }
    }

    fn downstream_target_files(&self, node_id: &NodeId) -> Vec<String> {
        let mut downstream_ids: HashSet<NodeId> = HashSet::new();
        downstream_ids.insert(node_id.clone());
        let mut grew = true;
        while grew {
            grew = false;
            for node in &self.nodes {
                if downstream_ids.contains(&node.id) {
                    continue;
                }
                if node
                    .dependencies
                    .iter()
                    .any(|dep| downstream_ids.contains(dep))
                {
                    downstream_ids.insert(node.id.clone());
                    grew = true;
                }
            }
        }
        downstream_ids.remove(node_id);

        let mut targets = self
            .nodes
            .iter()
            .filter(|node| downstream_ids.contains(&node.id))
            .flat_map(|node| node.target_files.iter().cloned())
            .collect::<Vec<_>>();
        targets.sort();
        targets.dedup();
        targets
    }

    pub(super) fn validate_required_tests_completed(&self) -> Result<(), String> {
        let is_work_like = |node: &&Node| node.kind == NodeKind::Work;

        let completed_targets: HashSet<&str> = self
            .nodes
            .iter()
            .filter(is_work_like)
            .filter(|node| node.status == NodeStatus::Completed)
            .flat_map(|node| node.target_files.iter().map(String::as_str))
            .collect();

        for node in self
            .nodes
            .iter()
            .filter(is_work_like)
            .filter(|node| node.status == NodeStatus::Completed)
        {
            for required in &node.required_validation_targets {
                if !completed_targets.contains(required.as_str()) {
                    return Err(format!(
                        "required test target '{required}' for node {} was not completed",
                        node.id.0
                    ));
                }
            }
        }

        Ok(())
    }

    // ── graph mutations ────────────────────────────────────────────────────────

    pub(super) fn mark_node(mut self, node_id: &NodeId, status: NodeStatus) -> RunGraph {
        for n in &mut self.nodes {
            if &n.id == node_id {
                n.status = status.clone();
            }
        }
        self
    }

    pub(super) fn mark_node_completed_with_summary(
        mut self,
        node_id: &NodeId,
        summary: String,
    ) -> RunGraph {
        for n in &mut self.nodes {
            if &n.id == node_id {
                n.status = NodeStatus::Completed;
                n.summary = Some(summary.clone());
            }
        }
        self
    }

    pub(super) fn push_node(mut self, node: Node) -> RunGraph {
        self.nodes.push(node);
        self
    }

    pub(super) fn remap_pending_dependencies(
        mut self,
        old_id: &NodeId,
        new_id: &NodeId,
    ) -> RunGraph {
        for n in &mut self.nodes {
            if n.status == NodeStatus::Pending {
                for dep in &mut n.dependencies {
                    if dep == old_id {
                        *dep = new_id.clone();
                    }
                }
            }
        }
        self
    }

    pub(super) fn cancel_pending_dependents(mut self, failed_id: &NodeId) -> RunGraph {
        let mut tainted: HashSet<NodeId> = HashSet::new();
        tainted.insert(failed_id.clone());

        loop {
            let mut grew = false;
            for node in &self.nodes {
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

        for n in &mut self.nodes {
            if tainted.contains(&n.id) {
                n.status = NodeStatus::Cancelled;
            }
        }
        self
    }

    pub(super) fn insert_children(
        mut self,
        parent_id: &NodeId,
        children: Vec<super::types::NodeRequest>,
    ) -> RunGraph {
        let parent_depth = self.get_node(parent_id).plan_depth;

        let local_to_graph: HashMap<NodeId, NodeId> = children
            .iter()
            .map(|req| (req.id.clone(), new_node_id()))
            .collect();

        for req in children {
            let id = local_to_graph[&req.id].clone();
            let plan_depth = plan_child_depth(parent_depth, &req.kind);
            let dependencies = req
                .dependencies
                .into_iter()
                .map(|dep| local_to_graph.get(&dep).cloned().unwrap_or(dep))
                .collect();
            self.nodes.push(Node {
                id,
                kind: req.kind,
                team: req.team,
                task_id: req.task_id,
                worker_role: req.worker_role,
                objective: req.objective,
                target_files: req.target_files,
                required_validation_targets: req.required_validation_targets,
                dependencies,
                status: NodeStatus::Pending,
                attempt: 0,
                plan_depth,
                model_tier: ModelTier::Cheap,
                summary: None,
                origin: NodeOrigin::PlanExpansion,
                validation_plan: req.validation_plan,
                retry_feedback: None,
            });
        }
        self
    }

    // ── validation ───────────────────────────────────────────────────────────

    pub(super) fn validate_plan_dependencies(
        &self,
        children: &[super::types::NodeRequest],
    ) -> Result<(), String> {
        let known: HashSet<&NodeId> = self.nodes.iter().map(|n| &n.id).collect();
        let sibling_ids: HashSet<&NodeId> = children.iter().map(|c| &c.id).collect();
        for child in children {
            for dep in &child.dependencies {
                if known.contains(dep) || sibling_ids.contains(dep) {
                    continue;
                }
                return Err(format!(
                    "plan output references unknown node id: {:?}",
                    dep.0
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_graph_invariants(&self) -> Result<(), String> {
        let mut seen: HashSet<&NodeId> = HashSet::new();
        for node in &self.nodes {
            if !seen.insert(&node.id) {
                return Err(format!("duplicate node id: {}", node.id.0));
            }
        }

        let all_ids: HashSet<&NodeId> = self.nodes.iter().map(|n| &n.id).collect();
        for node in &self.nodes {
            for dep in &node.dependencies {
                if !all_ids.contains(dep) {
                    return Err(format!(
                        "missing dependency: node {} depends on unknown id {}",
                        node.id.0, dep.0
                    ));
                }
            }
        }

        self.validate_origin_sources(&all_ids)?;

        Ok(())
    }

    pub(super) fn validate_origin_sources(&self, all_ids: &HashSet<&NodeId>) -> Result<(), String> {
        for node in &self.nodes {
            match &node.origin {
                NodeOrigin::Retry { source } => {
                    if !all_ids.contains(source) {
                        return Err(format!(
                            "missing origin source: node {} has Retry source {}",
                            node.id.0, source.0
                        ));
                    }
                }
                NodeOrigin::ElevateModel { source } => {
                    if !all_ids.contains(source) {
                        return Err(format!(
                            "missing origin source: node {} has ElevateModel source {}",
                            node.id.0, source.0
                        ));
                    }
                }
                NodeOrigin::Split { source } => {
                    if !all_ids.contains(source) {
                        return Err(format!(
                            "missing origin source: node {} has Split source {}",
                            node.id.0, source.0
                        ));
                    }
                }
                NodeOrigin::Root | NodeOrigin::PlanExpansion => {}
            }
        }
        Ok(())
    }

    pub(super) fn active_node(&self) -> Result<&Node, String> {
        let active = self.active_nodes();

        if active.is_empty() {
            return Err(
                "invalid waiting state: expected exactly one active node; found none".to_string(),
            );
        }

        if active.len() > 1 {
            let ids: Vec<String> = active.iter().map(|n| n.id.0.clone()).collect();
            return Err(format!(
                "invalid waiting state: multiple active nodes: {}",
                ids.join(", ")
            ));
        }

        Ok(active[0])
    }

    pub(super) fn diagnose_no_ready(&self) -> String {
        let existing: HashSet<&NodeId> = self.nodes.iter().map(|n| &n.id).collect();
        for node in &self.nodes {
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

    pub(super) fn invalid_node_return_reason(&self, node_id: &NodeId) -> Option<String> {
        match self.node_for_running(node_id) {
            None => Some(format!("node {} not found in graph", node_id.0)),
            Some(node) if node.status != NodeStatus::Running => Some(format!(
                "protocol violation: NodeReturned for node {} expected Running but found {:?}",
                node_id.0, node.status
            )),
            _ => None,
        }
    }

    pub(super) fn invalid_integration_reason(&self, node_id: &NodeId) -> Option<String> {
        match self.node_for_running(node_id) {
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

    pub(super) fn invalid_planner_task_integration_reason(
        &self,
        node_id: &NodeId,
    ) -> Option<String> {
        match self.node_for_running(node_id) {
            None => Some(format!("node {} not found in graph", node_id.0)),
            Some(node) if node.kind != NodeKind::Plan => Some(format!(
                "node {} is {:?} but PlannerTaskIntegrationReturned requires a Plan node",
                node_id.0, node.kind
            )),
            Some(node) if node.status != NodeStatus::Integrating => Some(format!(
                "node {} has status {:?} but PlannerTaskIntegrationReturned requires Integrating",
                node_id.0, node.status
            )),
            _ => None,
        }
    }

    /// Whether a non-terminal-failed node already exists for `team`, optionally
    /// scoped to a specific `task_id`.
    ///
    /// Used before inserting a team-trigger-spawned node so re-evaluating
    /// triggers on every node completion doesn't insert duplicates for a pair
    /// still in flight (no manifest row yet, so the manifest alone can't tell
    /// us it's already been requested).
    pub(super) fn has_active_team_node(&self, team: &str, task_id: Option<&str>) -> bool {
        self.nodes.iter().any(|n| {
            n.team == team && n.task_id.as_deref() == task_id && n.status != NodeStatus::Failed
        })
    }

    /// Count `(node_count, completed_count)` for a graph — used in telemetry.
    pub fn node_counts(&self) -> (usize, usize) {
        let completed = self
            .nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Completed)
            .count();
        (self.nodes.len(), completed)
    }
}

pub(super) fn attempts_exhausted(node: &Node) -> bool {
    node.attempt >= MAX_ATTEMPTS
}

/// Mints a fresh, random `NodeId`.
pub(super) fn new_node_id() -> NodeId {
    NodeId(Uuid::new_v4().to_string())
}

// ── depth/size helpers ─────────────────────────────────────────────────────────

pub(super) fn plan_child_depth(parent_depth: usize, kind: &NodeKind) -> usize {
    match kind {
        NodeKind::Plan => parent_depth + 1,
        NodeKind::Work => parent_depth,
    }
}

fn plan_depth_limit_reason(depth: usize) -> String {
    format!("plan depth limit exceeded: requested depth {depth}; limit is {MAX_PLAN_DEPTH}")
}

pub(super) fn validate_plan_child_depths(
    parent_depth: usize,
    children: &[super::types::NodeRequest],
) -> Result<(), String> {
    for child in children {
        let child_depth = plan_child_depth(parent_depth, &child.kind);
        if child_depth > MAX_PLAN_DEPTH {
            return Err(plan_depth_limit_reason(child_depth));
        }
    }
    Ok(())
}

pub(super) fn validate_split_depth(original_depth: usize) -> Result<(), String> {
    let split_depth = original_depth + 1;
    if split_depth > MAX_PLAN_DEPTH {
        Err(plan_depth_limit_reason(split_depth))
    } else {
        Ok(())
    }
}

pub(super) fn invalid_node_event_reason(
    node_id: &NodeId,
    node_kind: &NodeKind,
    event: &SchedulerEvent,
) -> Option<String> {
    match (node_kind, event) {
        (NodeKind::Work, SchedulerEvent::PlanAccepted { .. }) => Some(format!(
            "node {} is Work but received PlanAccepted outcome",
            node_id.0
        )),
        (NodeKind::Plan, SchedulerEvent::WorkAccepted { .. }) => Some(format!(
            "node {} is Plan but received WorkAccepted outcome",
            node_id.0
        )),
        _ => None,
    }
}
