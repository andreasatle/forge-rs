use super::*;

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
