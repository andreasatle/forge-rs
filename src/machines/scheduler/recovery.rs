//! Recovery application and routing for `SchedulerMachine`.
//!
//! Contains the graph-mutation helpers for the three recoverable outcomes
//! (Retry, ElevateModel, Split) and the `route_recovery` dispatcher that
//! selects among them based on the `RecoveryAction` emitted by a node.

use crate::engine::Transition;

use super::effect::SchedulerEffect;
use super::event::{FailureKind, RecoveryAction};
use super::graph::{
    MAX_ATTEMPTS, attempts_exhausted, cancel_pending_dependents, get_node, graph_has_capacity,
    graph_size_limit_reason, mark_node, push_node, remap_pending_dependencies,
    validate_split_depth,
};
use super::state::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, SchedulerState,
};

pub(super) fn failed_transition(
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

pub(super) fn apply_retry(
    graph: RunGraph,
    node_id: &NodeId,
    failure_kind: FailureKind,
    retry_message: &str,
) -> RunGraph {
    let (kind, objective, target_files, deps, attempt, plan_depth, model_tier) = {
        let n = get_node(&graph, node_id);
        (
            n.kind.clone(),
            n.objective.clone(),
            n.target_files.clone(),
            n.dependencies.clone(),
            n.attempt,
            n.plan_depth,
            n.model_tier,
        )
    };
    let objective = retry_objective(
        &kind,
        &objective,
        &target_files,
        failure_kind,
        retry_message,
    );
    let replacement_id = NodeId(format!("{}-retry-{}", node_id.0, graph.next_id));
    let replacement = Node {
        id: replacement_id.clone(),
        kind,
        objective,
        target_files,
        dependencies: deps,
        status: NodeStatus::Pending,
        attempt: attempt + 1,
        plan_depth,
        model_tier,
        summary: None,
        origin: NodeOrigin::Retry {
            source: node_id.clone(),
        },
    };
    let graph = mark_node(graph, node_id, NodeStatus::Failed);
    let graph = push_node(graph, replacement);
    remap_pending_dependencies(graph, node_id, &replacement_id)
}

const FEEDBACK_SEPARATOR: &str = "\n\nValidation feedback for retry:";

// Returns the objective text stripped of any previously appended feedback block.
fn base_objective(objective: &str) -> &str {
    match objective.find(FEEDBACK_SEPARATOR) {
        Some(idx) => objective[..idx].trim_end(),
        None => objective,
    }
}

fn retry_objective(
    kind: &NodeKind,
    original_objective: &str,
    target_files: &[String],
    failure_kind: FailureKind,
    retry_message: &str,
) -> String {
    if *kind != NodeKind::Work || failure_kind != FailureKind::ValidationFailure {
        return original_objective.to_string();
    }

    let clean_original = base_objective(original_objective);

    let target_text = if target_files.is_empty() {
        "(none specified)".to_string()
    } else {
        target_files.join(", ")
    };

    format!(
        "{clean_original}\n\nValidation feedback for retry:\nTarget files: {target_text}\n{diagnostics}",
        diagnostics = concise_retry_diagnostics(retry_message),
    )
}

fn concise_retry_diagnostics(message: &str) -> String {
    const LIMIT: usize = 4000;
    let trimmed = message.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(LIMIT).collect();
    out.push_str("\n[diagnostics truncated]");
    out
}

pub(super) fn apply_split(graph: RunGraph, node_id: &NodeId, message: String) -> RunGraph {
    let (target_files, deps, attempt, plan_depth) = {
        let n = get_node(&graph, node_id);
        (
            n.target_files.clone(),
            n.dependencies.clone(),
            n.attempt,
            n.plan_depth + 1,
        )
    };
    let split_id = NodeId(format!("{}-split-{}", node_id.0, graph.next_id));
    let split_node = Node {
        id: split_id.clone(),
        kind: NodeKind::Plan,
        objective: message,
        target_files,
        dependencies: deps,
        status: NodeStatus::Pending,
        attempt: attempt + 1,
        plan_depth,
        model_tier: ModelTier::Strong,
        summary: None,
        origin: NodeOrigin::Split {
            source: node_id.clone(),
        },
    };
    let graph = mark_node(graph, node_id, NodeStatus::Failed);
    let graph = push_node(graph, split_node);
    remap_pending_dependencies(graph, node_id, &split_id)
}

pub(super) fn apply_elevate(graph: RunGraph, node_id: &NodeId) -> RunGraph {
    let (kind, objective, target_files, deps, attempt, plan_depth) = {
        let n = get_node(&graph, node_id);
        (
            n.kind.clone(),
            n.objective.clone(),
            n.target_files.clone(),
            n.dependencies.clone(),
            n.attempt,
            n.plan_depth,
        )
    };
    let elevated_id = NodeId(format!("{}-elevated-{}", node_id.0, graph.next_id));
    let replacement = Node {
        id: elevated_id.clone(),
        kind,
        objective,
        target_files,
        dependencies: deps,
        status: NodeStatus::Pending,
        attempt: attempt + 1,
        plan_depth,
        model_tier: ModelTier::Strong,
        summary: None,
        origin: NodeOrigin::ElevateModel {
            source: node_id.clone(),
        },
    };
    let graph = mark_node(graph, node_id, NodeStatus::Failed);
    let graph = push_node(graph, replacement);
    remap_pending_dependencies(graph, node_id, &elevated_id)
}

pub(super) fn route_recovery(
    has_strong_tier: bool,
    graph: RunGraph,
    node_id: &NodeId,
    failure_kind: FailureKind,
    failure_reason: String,
    recovery: RecoveryAction,
) -> Transition<SchedulerState, SchedulerEffect> {
    match recovery {
        RecoveryAction::Retry { message } => {
            let exhausted = attempts_exhausted(get_node(&graph, node_id));
            if exhausted {
                let reason = format!(
                    "node {} exhausted all {} attempts (Retry)",
                    node_id.0, MAX_ATTEMPTS
                );
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph: graph.clone(),
                        reason: reason.clone(),
                    },
                    effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                }
            } else if !graph_has_capacity(&graph, 1) {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(graph, graph_size_limit_reason(1))
            } else {
                let graph = apply_retry(graph, node_id, failure_kind, &message);
                Transition {
                    state: SchedulerState::Running { graph },
                    effects: vec![],
                }
            }
        }

        RecoveryAction::Split { message } => {
            let node = get_node(&graph, node_id);
            let exhausted = attempts_exhausted(node);
            let split_depth_result = validate_split_depth(node.plan_depth);
            if exhausted {
                let reason = format!(
                    "node {} exhausted all {} attempts (Split)",
                    node_id.0, MAX_ATTEMPTS
                );
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph: graph.clone(),
                        reason: reason.clone(),
                    },
                    effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                }
            } else if !graph_has_capacity(&graph, 1) {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(graph, graph_size_limit_reason(1))
            } else if let Err(reason) = split_depth_result {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(graph, reason)
            } else {
                let graph = apply_split(graph, node_id, message);
                Transition {
                    state: SchedulerState::Running { graph },
                    effects: vec![],
                }
            }
        }

        RecoveryAction::ElevateModel { .. } => {
            let (can_elevate, exhausted) = {
                let node = get_node(&graph, node_id);
                let can = has_strong_tier && node.model_tier == ModelTier::Cheap;
                (can, attempts_exhausted(node))
            };

            if !can_elevate {
                if exhausted {
                    let reason = format!(
                        "node {} exhausted all {} attempts; no higher model tier available",
                        node_id.0, MAX_ATTEMPTS
                    );
                    let graph = mark_node(graph, node_id, NodeStatus::Failed);
                    Transition {
                        state: SchedulerState::Failed {
                            graph: graph.clone(),
                            reason: reason.clone(),
                        },
                        effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                    }
                } else if !graph_has_capacity(&graph, 1) {
                    let graph = mark_node(graph, node_id, NodeStatus::Failed);
                    failed_transition(graph, graph_size_limit_reason(1))
                } else {
                    let graph = apply_retry(graph, node_id, failure_kind, "");
                    Transition {
                        state: SchedulerState::Running { graph },
                        effects: vec![],
                    }
                }
            } else if exhausted {
                let reason = format!(
                    "node {} exhausted all {} attempts (ElevateModel)",
                    node_id.0, MAX_ATTEMPTS
                );
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph: graph.clone(),
                        reason: reason.clone(),
                    },
                    effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
                }
            } else if !graph_has_capacity(&graph, 1) {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(graph, graph_size_limit_reason(1))
            } else {
                let graph = apply_elevate(graph, node_id);
                Transition {
                    state: SchedulerState::Running { graph },
                    effects: vec![],
                }
            }
        }

        RecoveryAction::Terminal { message } => {
            let reason = terminal_failure_reason(&failure_reason, &message);
            let graph = mark_node(graph, node_id, NodeStatus::Failed);
            let graph = cancel_pending_dependents(graph, node_id);
            Transition {
                state: SchedulerState::Failed {
                    graph: graph.clone(),
                    reason: reason.clone(),
                },
                effects: vec![SchedulerEffect::ReturnFailed { graph, reason }],
            }
        }
    }
}

pub(super) fn terminal_failure_reason(failure_reason: &str, terminal_message: &str) -> String {
    if terminal_message.is_empty() {
        return failure_reason.to_string();
    }
    if failure_reason.is_empty()
        || terminal_message == failure_reason
        || terminal_message.contains(failure_reason)
    {
        return terminal_message.to_string();
    }
    if failure_reason.contains(terminal_message) {
        return failure_reason.to_string();
    }
    format!("{terminal_message}: {failure_reason}")
}
