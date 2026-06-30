//! Recovery application and routing for `SchedulerMachine`.
//!
//! Contains the graph-mutation helpers for the three recoverable outcomes
//! (Retry, ElevateModel, Split) and the `route_recovery` dispatcher that
//! selects among them based on the `RecoveryAction` emitted by a node.

use crate::engine::Transition;

use super::effect::SchedulerEffect;
use super::event::{FailureKind, RecoveryAction};
use super::graph::{
    MAX_ATTEMPTS, MAX_GRAPH_NODES, MAX_PLAN_DEPTH, attempts_exhausted, cancel_pending_dependents,
    get_node, graph_has_capacity, mark_node, push_node, remap_pending_dependencies,
    validate_split_depth,
};
use super::graph::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RetryFeedback, RunGraph,
};
use super::state::{ExhaustedAction, FailureReason, RunConfig, SchedulerState};

pub(super) fn failed_transition(
    graph: RunGraph,
    reason: FailureReason,
) -> Transition<SchedulerState, SchedulerEffect> {
    Transition {
        state: SchedulerState::Failed { graph, reason },
        effects: vec![],
    }
}

pub(super) fn apply_retry(
    graph: RunGraph,
    node_id: &NodeId,
    failure_kind: FailureKind,
    retry_message: &str,
) -> RunGraph {
    let (
        kind,
        objective,
        target_files,
        required_test_targets,
        deps,
        attempt,
        plan_depth,
        model_tier,
        validation_plan,
    ) = {
        let n = get_node(&graph, node_id);
        (
            n.kind.clone(),
            n.objective.clone(),
            n.target_files.clone(),
            n.required_test_targets.clone(),
            n.dependencies.clone(),
            n.attempt,
            n.plan_depth,
            n.model_tier,
            n.validation_plan.clone(),
        )
    };
    let retry_feedback = build_retry_feedback(&kind, failure_kind, retry_message);
    let replacement_id = NodeId(format!("{}-retry-{}", node_id.0, graph.next_id));
    let replacement = Node {
        id: replacement_id.clone(),
        kind,
        objective,
        target_files,
        required_test_targets,
        dependencies: deps,
        status: NodeStatus::Pending,
        attempt: attempt + 1,
        plan_depth,
        model_tier,
        summary: None,
        origin: NodeOrigin::Retry {
            source: node_id.clone(),
        },
        validation_plan,
        retry_feedback,
    };
    let graph = mark_node(graph, node_id, NodeStatus::Failed);
    let graph = push_node(graph, replacement);
    remap_pending_dependencies(graph, node_id, &replacement_id)
}

/// Builds `RetryFeedback` for validation-class failures on Work nodes.
///
/// Only `ValidationFailure` and `WorkSemanticValidationFailure` on `Work` nodes
/// receive feedback; all other failure kinds return `None` so the objective
/// stays clean.
fn build_retry_feedback(
    kind: &NodeKind,
    failure_kind: FailureKind,
    retry_message: &str,
) -> Option<RetryFeedback> {
    if *kind != NodeKind::Work
        || !matches!(
            failure_kind,
            FailureKind::ValidationFailure | FailureKind::WorkSemanticValidationFailure
        )
    {
        return None;
    }
    Some(RetryFeedback {
        diagnostics: concise_retry_diagnostics(retry_message),
    })
}

fn concise_retry_diagnostics(message: &str) -> String {
    const LIMIT: usize = 1200;
    const MAX_LINES: usize = 12;
    let trimmed = message.trim();
    let mut out = trimmed
        .lines()
        .take(MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if out.chars().count() > LIMIT {
        out = out.chars().take(LIMIT).collect();
        out.push_str("\n[diagnostics truncated]");
    } else if trimmed.lines().count() > MAX_LINES {
        out.push_str("\n[diagnostics truncated]");
    }
    out
}

pub(super) fn apply_split(graph: RunGraph, node_id: &NodeId, message: String) -> RunGraph {
    let (target_files, required_test_targets, deps, attempt, plan_depth) = {
        let n = get_node(&graph, node_id);
        (
            n.target_files.clone(),
            n.required_test_targets.clone(),
            n.dependencies.clone(),
            n.attempt,
            n.plan_depth + 1,
        )
    };
    let split_id = NodeId(format!("{}-split-{}", node_id.0, graph.next_id));
    // Split creates a new Plan node; validation_plan and retry_feedback belong to Work nodes only.
    let split_node = Node {
        id: split_id.clone(),
        kind: NodeKind::Plan,
        objective: message,
        target_files,
        required_test_targets,
        dependencies: deps,
        status: NodeStatus::Pending,
        attempt: attempt + 1,
        plan_depth,
        model_tier: ModelTier::Strong,
        summary: None,
        origin: NodeOrigin::Split {
            source: node_id.clone(),
        },
        validation_plan: None,
        retry_feedback: None,
    };
    let graph = mark_node(graph, node_id, NodeStatus::Failed);
    let graph = push_node(graph, split_node);
    remap_pending_dependencies(graph, node_id, &split_id)
}

pub(super) fn apply_elevate(graph: RunGraph, node_id: &NodeId) -> RunGraph {
    let (
        kind,
        objective,
        target_files,
        required_test_targets,
        deps,
        attempt,
        plan_depth,
        validation_plan,
    ) = {
        let n = get_node(&graph, node_id);
        (
            n.kind.clone(),
            n.objective.clone(),
            n.target_files.clone(),
            n.required_test_targets.clone(),
            n.dependencies.clone(),
            n.attempt,
            n.plan_depth,
            n.validation_plan.clone(),
        )
    };
    let elevated_id = NodeId(format!("{}-elevated-{}", node_id.0, graph.next_id));
    let replacement = Node {
        id: elevated_id.clone(),
        kind,
        objective,
        target_files,
        required_test_targets,
        dependencies: deps,
        status: NodeStatus::Pending,
        attempt: attempt + 1,
        plan_depth,
        model_tier: ModelTier::Strong,
        summary: None,
        origin: NodeOrigin::ElevateModel {
            source: node_id.clone(),
        },
        validation_plan,
        retry_feedback: None,
    };
    let graph = mark_node(graph, node_id, NodeStatus::Failed);
    let graph = push_node(graph, replacement);
    remap_pending_dependencies(graph, node_id, &elevated_id)
}

pub(super) fn route_recovery(
    run_config: RunConfig,
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
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph,
                        reason: FailureReason::AttemptsExhausted {
                            node_id: node_id.0.clone(),
                            max_attempts: MAX_ATTEMPTS,
                            recovery_action: ExhaustedAction::Retry,
                        },
                    },
                    effects: vec![],
                }
            } else if !graph_has_capacity(&graph, 1) {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(
                    graph,
                    FailureReason::GraphCapacityExceeded {
                        limit: MAX_GRAPH_NODES,
                    },
                )
            } else {
                let graph = apply_retry(graph, node_id, failure_kind, &message);
                Transition {
                    state: SchedulerState::Active { graph, run_config },
                    effects: vec![],
                }
            }
        }

        RecoveryAction::Split { message } => {
            let node = get_node(&graph, node_id);
            let exhausted = attempts_exhausted(node);
            let split_depth_ok = validate_split_depth(node.plan_depth).is_ok();
            if exhausted {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph,
                        reason: FailureReason::AttemptsExhausted {
                            node_id: node_id.0.clone(),
                            max_attempts: MAX_ATTEMPTS,
                            recovery_action: ExhaustedAction::Split,
                        },
                    },
                    effects: vec![],
                }
            } else if !graph_has_capacity(&graph, 1) {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(
                    graph,
                    FailureReason::GraphCapacityExceeded {
                        limit: MAX_GRAPH_NODES,
                    },
                )
            } else if !split_depth_ok {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(
                    graph,
                    FailureReason::PlanDepthExceeded {
                        limit: MAX_PLAN_DEPTH,
                    },
                )
            } else {
                let graph = apply_split(graph, node_id, message);
                Transition {
                    state: SchedulerState::Active { graph, run_config },
                    effects: vec![],
                }
            }
        }

        RecoveryAction::ElevateModel { .. } => {
            let (can_elevate, exhausted) = {
                let node = get_node(&graph, node_id);
                let can = run_config.has_strong_tier && node.model_tier == ModelTier::Cheap;
                (can, attempts_exhausted(node))
            };

            if !can_elevate {
                if exhausted {
                    let graph = mark_node(graph, node_id, NodeStatus::Failed);
                    Transition {
                        state: SchedulerState::Failed {
                            graph,
                            reason: FailureReason::NoHigherModelTierAvailable {
                                node_id: node_id.0.clone(),
                                max_attempts: MAX_ATTEMPTS,
                            },
                        },
                        effects: vec![],
                    }
                } else if !graph_has_capacity(&graph, 1) {
                    let graph = mark_node(graph, node_id, NodeStatus::Failed);
                    failed_transition(
                        graph,
                        FailureReason::GraphCapacityExceeded {
                            limit: MAX_GRAPH_NODES,
                        },
                    )
                } else {
                    let graph = apply_retry(graph, node_id, failure_kind, "");
                    Transition {
                        state: SchedulerState::Active { graph, run_config },
                        effects: vec![],
                    }
                }
            } else if exhausted {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                Transition {
                    state: SchedulerState::Failed {
                        graph,
                        reason: FailureReason::AttemptsExhausted {
                            node_id: node_id.0.clone(),
                            max_attempts: MAX_ATTEMPTS,
                            recovery_action: ExhaustedAction::ElevateModel,
                        },
                    },
                    effects: vec![],
                }
            } else if !graph_has_capacity(&graph, 1) {
                let graph = mark_node(graph, node_id, NodeStatus::Failed);
                failed_transition(
                    graph,
                    FailureReason::GraphCapacityExceeded {
                        limit: MAX_GRAPH_NODES,
                    },
                )
            } else {
                let graph = apply_elevate(graph, node_id);
                Transition {
                    state: SchedulerState::Active { graph, run_config },
                    effects: vec![],
                }
            }
        }

        RecoveryAction::Terminal { message } => {
            let graph = mark_node(graph, node_id, NodeStatus::Failed);
            let graph = cancel_pending_dependents(graph, node_id);
            Transition {
                state: SchedulerState::Failed {
                    graph,
                    reason: FailureReason::TerminalRecovery {
                        failure_message: failure_reason,
                        terminal_message: message,
                    },
                },
                effects: vec![],
            }
        }
    }
}
