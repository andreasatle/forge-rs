//! Pure graph inspection and mutation helpers for `SchedulerMachine`.
//!
//! All functions here take and return `RunGraph` values; none hold references to
//! `SchedulerMachine` itself. They are free functions rather than methods so
//! that `machine.rs` and `recovery.rs` can call them without going through
//! `Self`.

use std::collections::{HashMap, HashSet};

use super::event::{NodeOutcome, NodeOutcome::*};
use super::state::{ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph};

/// Maximum number of attempts allowed per objective before recovery stops.
pub(super) const MAX_ATTEMPTS: u32 = 3;

/// Scheduler circuit breaker for graph growth.
pub(super) const MAX_GRAPH_NODES: usize = 100;

/// Scheduler circuit breaker for recursive planning depth.
pub(super) const MAX_PLAN_DEPTH: usize = 10;

// ── graph queries ──────────────────────────────────────────────────────────────

pub(super) fn find_ready(graph: &RunGraph) -> Vec<NodeId> {
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

pub(super) fn all_complete(graph: &RunGraph) -> bool {
    !graph.nodes.iter().any(|n| {
        matches!(
            n.status,
            NodeStatus::Pending | NodeStatus::Running | NodeStatus::Integrating
        )
    })
}

pub(super) fn get_node<'a>(graph: &'a RunGraph, node_id: &NodeId) -> &'a Node {
    graph
        .nodes
        .iter()
        .find(|n| &n.id == node_id)
        .expect("node not found in graph")
}

pub(super) fn node_for_running<'a>(graph: &'a RunGraph, node_id: &NodeId) -> Option<&'a Node> {
    graph.nodes.iter().find(|n| &n.id == node_id)
}

pub(super) fn active_nodes(graph: &RunGraph) -> Vec<&Node> {
    graph
        .nodes
        .iter()
        .filter(|n| matches!(n.status, NodeStatus::Running | NodeStatus::Integrating))
        .collect()
}

pub(super) fn attempts_exhausted(node: &Node) -> bool {
    node.attempt >= MAX_ATTEMPTS
}

pub(super) fn graph_has_capacity(graph: &RunGraph, additional_nodes: usize) -> bool {
    graph
        .nodes
        .len()
        .checked_add(additional_nodes)
        .is_some_and(|total| total <= MAX_GRAPH_NODES)
}

// ── graph mutations ────────────────────────────────────────────────────────────

pub(super) fn mark_node(graph: RunGraph, node_id: &NodeId, status: NodeStatus) -> RunGraph {
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

pub(super) fn mark_node_completed_with_summary(
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

pub(super) fn push_node(mut graph: RunGraph, node: Node) -> RunGraph {
    graph.nodes.push(node);
    graph.next_id += 1;
    graph
}

pub(super) fn remap_pending_dependencies(
    graph: RunGraph,
    old_id: &NodeId,
    new_id: &NodeId,
) -> RunGraph {
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

pub(super) fn cancel_pending_dependents(graph: RunGraph, failed_id: &NodeId) -> RunGraph {
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

pub(super) fn insert_children(
    mut graph: RunGraph,
    parent_id: &NodeId,
    children: Vec<super::event::NodeRequest>,
) -> RunGraph {
    let parent_depth = get_node(&graph, parent_id).plan_depth;

    let local_to_graph: HashMap<NodeId, NodeId> = children
        .iter()
        .enumerate()
        .map(|(i, req)| {
            let graph_id = NodeId(format!(
                "{}-child-{}",
                parent_id.0,
                graph.next_id + i as u32
            ));
            (req.id.clone(), graph_id)
        })
        .collect();

    for req in children {
        let id = NodeId(format!("{}-child-{}", parent_id.0, graph.next_id));
        graph.next_id += 1;
        let plan_depth = plan_child_depth(parent_depth, &req.kind);
        let dependencies = req
            .dependencies
            .into_iter()
            .map(|dep| local_to_graph.get(&dep).cloned().unwrap_or(dep))
            .collect();
        graph.nodes.push(Node {
            id,
            kind: req.kind,
            objective: req.objective,
            target_files: req.target_files,
            dependencies,
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::PlanExpansion,
        });
    }
    graph
}

// ── depth/size helpers ─────────────────────────────────────────────────────────

pub(super) fn plan_child_depth(parent_depth: usize, kind: &NodeKind) -> usize {
    match kind {
        NodeKind::Plan => parent_depth + 1,
        NodeKind::Work => parent_depth,
    }
}

pub(super) fn graph_size_limit_reason(additional_nodes: usize) -> String {
    format!(
        "graph size limit exceeded: cannot add {additional_nodes} node(s); limit is {MAX_GRAPH_NODES}"
    )
}

pub(super) fn plan_depth_limit_reason(depth: usize) -> String {
    format!("plan depth limit exceeded: requested depth {depth}; limit is {MAX_PLAN_DEPTH}")
}

// ── validation ─────────────────────────────────────────────────────────────────

pub(super) fn validate_plan_child_depths(
    parent_depth: usize,
    children: &[super::event::NodeRequest],
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

pub(super) fn validate_plan_dependencies(
    graph: &RunGraph,
    children: &[super::event::NodeRequest],
) -> Result<(), String> {
    let known: HashSet<&NodeId> = graph.nodes.iter().map(|n| &n.id).collect();
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

pub(super) fn validate_graph_invariants(graph: &RunGraph) -> Result<(), String> {
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

    validate_origin_sources(graph, &all_ids)?;

    Ok(())
}

pub(super) fn validate_origin_sources(
    graph: &RunGraph,
    all_ids: &HashSet<&NodeId>,
) -> Result<(), String> {
    for node in &graph.nodes {
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

pub(super) fn validate_waiting_state(graph: &RunGraph, running: &NodeId) -> Result<(), String> {
    let running_node = match graph.nodes.iter().find(|n| &n.id == running) {
        None => {
            return Err(format!(
                "invalid waiting state: running node missing node id {}",
                running.0
            ));
        }
        Some(n) => n,
    };

    let active = active_nodes(graph);

    if active.is_empty() {
        return Err(format!(
            "invalid waiting state: expected active node for {}; found none (status: {:?})",
            running.0, running_node.status
        ));
    }

    if active.len() > 1 {
        let ids: Vec<String> = active.iter().map(|n| n.id.0.clone()).collect();
        return Err(format!(
            "invalid waiting state: multiple active nodes: {}",
            ids.join(", ")
        ));
    }

    if active[0].id != *running {
        return Err(format!(
            "invalid waiting state: active node is {} but waiting.running is {}",
            active[0].id.0, running.0
        ));
    }

    Ok(())
}

pub(super) fn diagnose_no_ready(graph: &RunGraph) -> String {
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

pub(super) fn invalid_node_outcome_reason(
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

pub(super) fn invalid_node_return_reason(graph: &RunGraph, node_id: &NodeId) -> Option<String> {
    match node_for_running(graph, node_id) {
        None => Some(format!("node {} not found in graph", node_id.0)),
        Some(node) if node.status != NodeStatus::Running => Some(format!(
            "protocol violation: NodeReturned for node {} expected Running but found {:?}",
            node_id.0, node.status
        )),
        _ => None,
    }
}

pub(super) fn invalid_integration_reason(graph: &RunGraph, node_id: &NodeId) -> Option<String> {
    match node_for_running(graph, node_id) {
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
