//! Recovery application and routing for `SchedulerMachine`.
//!
//! Contains the graph-mutation helpers for the three recoverable outcomes
//! (Retry, ElevateModel, Split) and the `route_recovery` dispatcher that
//! selects among them based on the `RecoveryAction` emitted by a node.

use crate::engine::Transition;

use super::config::RunConfig;
use super::effect::SchedulerEffect;
use super::failure::{ExhaustedAction, FailureKind, FailureReason};
use super::graph::{
    MAX_ATTEMPTS, MAX_GRAPH_NODES, attempts_exhausted, new_node_id, validate_split_depth,
};
use super::graph::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RetryFeedback, RunGraph,
};
use super::state::SchedulerState;
use super::types::RecoveryAction;

pub(super) fn failed_transition(
    graph: RunGraph,
    reason: FailureReason,
) -> Transition<SchedulerState, SchedulerEffect> {
    Transition {
        state: SchedulerState::Failed { graph, reason },
        effects: vec![],
    }
}

pub(super) fn route_recovery(
    run_config: RunConfig,
    graph: RunGraph,
    node_id: &NodeId,
    failure_kind: FailureKind,
    failure_reason: String,
    recovery: RecoveryAction,
) -> Transition<SchedulerState, SchedulerEffect> {
    RecoveryApplicator::new(
        run_config,
        graph,
        node_id.clone(),
        failure_kind,
        failure_reason,
    )
    .route(recovery)
}

pub(super) struct RecoveryApplicator {
    run_config: RunConfig,
    graph: RunGraph,
    node_id: NodeId,
    failure_kind: FailureKind,
    failure_reason: String,
}

impl RecoveryApplicator {
    fn new(
        run_config: RunConfig,
        graph: RunGraph,
        node_id: NodeId,
        failure_kind: FailureKind,
        failure_reason: String,
    ) -> Self {
        Self {
            run_config,
            graph,
            node_id,
            failure_kind,
            failure_reason,
        }
    }

    fn route(self, recovery: RecoveryAction) -> Transition<SchedulerState, SchedulerEffect> {
        match recovery {
            RecoveryAction::Retry { message } => {
                let exhausted = attempts_exhausted(self.graph.get_node(&self.node_id));
                if exhausted {
                    let failed_node_id = self.node_id.0.clone();
                    let graph = self.mark_failed();
                    Transition {
                        state: SchedulerState::Failed {
                            graph,
                            reason: FailureReason::AttemptsExhausted {
                                node_id: failed_node_id,
                                max_attempts: MAX_ATTEMPTS,
                                recovery_action: ExhaustedAction::Retry,
                            },
                        },
                        effects: vec![],
                    }
                } else if !self.graph.graph_has_capacity(1) {
                    let graph = self.mark_failed();
                    failed_transition(
                        graph,
                        FailureReason::GraphCapacityExceeded {
                            limit: MAX_GRAPH_NODES,
                        },
                    )
                } else {
                    let run_config = self.run_config.clone();
                    let graph = self.apply_retry(&message);
                    Transition {
                        state: SchedulerState::Active { graph, run_config },
                        effects: vec![],
                    }
                }
            }

            RecoveryAction::Split { message } => {
                let node = self.graph.get_node(&self.node_id);
                let exhausted = attempts_exhausted(node);
                let split_depth_ok = validate_split_depth(node.plan_depth).is_ok();
                if exhausted {
                    let failed_node_id = self.node_id.0.clone();
                    let graph = self.mark_failed();
                    Transition {
                        state: SchedulerState::Failed {
                            graph,
                            reason: FailureReason::AttemptsExhausted {
                                node_id: failed_node_id,
                                max_attempts: MAX_ATTEMPTS,
                                recovery_action: ExhaustedAction::Split,
                            },
                        },
                        effects: vec![],
                    }
                } else if !split_depth_ok || !self.graph.graph_has_capacity(1) {
                    // Splitting further isn't viable; fall back to strengthening
                    // the model on the same task instead of failing outright.
                    self.elevate_model()
                } else {
                    let run_config = self.run_config.clone();
                    let graph = self.apply_split(message);
                    Transition {
                        state: SchedulerState::Active { graph, run_config },
                        effects: vec![],
                    }
                }
            }

            RecoveryAction::ElevateModel { .. } => self.elevate_model(),

            RecoveryAction::Terminal { message } => {
                let graph = self.graph.mark_node(&self.node_id, NodeStatus::Failed);
                let graph = graph.cancel_pending_dependents(&self.node_id);
                Transition {
                    state: SchedulerState::Failed {
                        graph,
                        reason: FailureReason::TerminalRecovery {
                            failure_message: self.failure_reason,
                            terminal_message: message,
                        },
                    },
                    effects: vec![],
                }
            }
        }
    }

    fn mark_failed(self) -> RunGraph {
        self.graph.mark_node(&self.node_id, NodeStatus::Failed)
    }

    /// Applies `ElevateModel` recovery, also used as the fallback when `Split`
    /// is not viable (plan depth limit reached or graph at capacity).
    fn elevate_model(self) -> Transition<SchedulerState, SchedulerEffect> {
        let (can_elevate, exhausted) = {
            let node = self.graph.get_node(&self.node_id);
            let can = self.run_config.has_strong_tier && node.model_tier == ModelTier::Cheap;
            (can, attempts_exhausted(node))
        };

        if !can_elevate {
            if exhausted {
                let failed_node_id = self.node_id.0.clone();
                let graph = self.mark_failed();
                Transition {
                    state: SchedulerState::Failed {
                        graph,
                        reason: FailureReason::NoHigherModelTierAvailable {
                            node_id: failed_node_id,
                            max_attempts: MAX_ATTEMPTS,
                        },
                    },
                    effects: vec![],
                }
            } else if !self.graph.graph_has_capacity(1) {
                let graph = self.mark_failed();
                failed_transition(
                    graph,
                    FailureReason::GraphCapacityExceeded {
                        limit: MAX_GRAPH_NODES,
                    },
                )
            } else {
                let run_config = self.run_config.clone();
                let graph = self.apply_retry("");
                Transition {
                    state: SchedulerState::Active { graph, run_config },
                    effects: vec![],
                }
            }
        } else if exhausted {
            let failed_node_id = self.node_id.0.clone();
            let graph = self.mark_failed();
            Transition {
                state: SchedulerState::Failed {
                    graph,
                    reason: FailureReason::AttemptsExhausted {
                        node_id: failed_node_id,
                        max_attempts: MAX_ATTEMPTS,
                        recovery_action: ExhaustedAction::ElevateModel,
                    },
                },
                effects: vec![],
            }
        } else if !self.graph.graph_has_capacity(1) {
            let graph = self.mark_failed();
            failed_transition(
                graph,
                FailureReason::GraphCapacityExceeded {
                    limit: MAX_GRAPH_NODES,
                },
            )
        } else {
            let run_config = self.run_config.clone();
            let graph = self.apply_elevate();
            Transition {
                state: SchedulerState::Active { graph, run_config },
                effects: vec![],
            }
        }
    }

    fn apply_retry(self, retry_message: &str) -> RunGraph {
        let (
            kind,
            worker_role,
            objective,
            target_files,
            required_validation_targets,
            deps,
            attempt,
            plan_depth,
            model_tier,
            validation_plan,
        ) = {
            let n = self.graph.get_node(&self.node_id);
            (
                n.kind.clone(),
                n.worker_role.clone(),
                n.objective.clone(),
                n.target_files.clone(),
                n.required_validation_targets.clone(),
                n.dependencies.clone(),
                n.attempt,
                n.plan_depth,
                n.model_tier,
                n.validation_plan.clone(),
            )
        };
        let retry_feedback = self.build_retry_feedback(&kind, retry_message);
        let replacement_id = new_node_id();
        let replacement = Node {
            id: replacement_id.clone(),
            kind,
            worker_role,
            objective,
            target_files,
            required_validation_targets,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            plan_depth,
            model_tier,
            summary: None,
            origin: NodeOrigin::Retry {
                source: self.node_id.clone(),
            },
            validation_plan,
            retry_feedback,
        };
        self.push_replacement(replacement, &replacement_id)
    }

    fn apply_split(self, message: String) -> RunGraph {
        let (objective, target_files, required_validation_targets, deps, attempt, plan_depth) = {
            let n = self.graph.get_node(&self.node_id);
            (
                n.objective.clone(),
                n.target_files.clone(),
                n.required_validation_targets.clone(),
                n.dependencies.clone(),
                n.attempt,
                n.plan_depth + 1,
            )
        };
        let split_id = new_node_id();
        // Split creates a new Plan node to re-plan the original objective;
        // `message` is diagnostic context, not the objective itself. The new
        // node re-plans the same task, so its objective is preserved and the
        // diagnostic is appended for the planner's benefit. validation_plan
        // and retry_feedback belong to Work nodes only.
        let split_node = Node {
            id: split_id.clone(),
            kind: NodeKind::Plan,
            worker_role: None,
            objective: format!(
                "{objective}\n\nThe previous attempt failed and requires decomposition:\n{message}"
            ),
            target_files,
            required_validation_targets,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            plan_depth,
            model_tier: ModelTier::Strong,
            summary: None,
            origin: NodeOrigin::Split {
                source: self.node_id.clone(),
            },
            validation_plan: None,
            retry_feedback: None,
        };
        self.push_replacement(split_node, &split_id)
    }

    fn apply_elevate(self) -> RunGraph {
        let (
            kind,
            worker_role,
            objective,
            target_files,
            required_validation_targets,
            deps,
            attempt,
            plan_depth,
            validation_plan,
        ) = {
            let n = self.graph.get_node(&self.node_id);
            (
                n.kind.clone(),
                n.worker_role.clone(),
                n.objective.clone(),
                n.target_files.clone(),
                n.required_validation_targets.clone(),
                n.dependencies.clone(),
                n.attempt,
                n.plan_depth,
                n.validation_plan.clone(),
            )
        };
        let elevated_id = new_node_id();
        let replacement = Node {
            id: elevated_id.clone(),
            kind,
            worker_role,
            objective,
            target_files,
            required_validation_targets,
            dependencies: deps,
            status: NodeStatus::Pending,
            attempt: attempt + 1,
            plan_depth,
            model_tier: ModelTier::Strong,
            summary: None,
            origin: NodeOrigin::ElevateModel {
                source: self.node_id.clone(),
            },
            validation_plan,
            retry_feedback: None,
        };
        self.push_replacement(replacement, &elevated_id)
    }

    fn push_replacement(self, replacement: Node, replacement_id: &NodeId) -> RunGraph {
        let graph = self.graph.mark_node(&self.node_id, NodeStatus::Failed);
        let graph = graph.push_node(replacement);
        graph.remap_pending_dependencies(&self.node_id, replacement_id)
    }

    /// Builds `RetryFeedback` for validation-class failures on Work nodes.
    ///
    /// Only `ValidationFailure` and `WorkSemanticValidationFailure` on `Work` nodes
    /// receive feedback; all other failure kinds return `None` so the objective
    /// stays clean.
    fn build_retry_feedback(&self, kind: &NodeKind, retry_message: &str) -> Option<RetryFeedback> {
        if *kind != NodeKind::Work
            || !matches!(
                self.failure_kind,
                FailureKind::ValidationFailure | FailureKind::WorkSemanticValidationFailure
            )
        {
            return None;
        }
        Some(RetryFeedback {
            diagnostics: Self::concise_retry_diagnostics(retry_message),
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
}
