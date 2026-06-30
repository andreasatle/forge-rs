use super::*;

#[test]
fn plan_child_depth_limit_fails_scheduler() {
    let mut graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    graph.nodes[0].plan_depth = MAX_PLAN_DEPTH;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("P".to_string()),
            outcome: NodeOutcome::PlanAccepted(PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("nested-plan".to_string()),
                    kind: NodeKind::Plan,
                    objective: "nested plan".to_string(),
                    target_files: vec![],
                    required_test_targets: vec![],
                    dependencies: vec![NodeId("P".to_string())],
                    validation_plan: None,
                }],
            }),
        },
    );

    let SchedulerState::Failed { graph, reason } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 1, "must not insert child plan");
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert!(
        reason.contains("plan depth limit") && reason.contains(&MAX_PLAN_DEPTH.to_string()),
        "unexpected reason: {reason:?}"
    );
    assert!(t.effects.is_empty());
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
fn plan_expansion_respects_graph_size_limit() {
    let graph = graph_with_filler_nodes(plan_node("P", "plan something", &[]), MAX_GRAPH_NODES - 1);
    let graph = running(graph, "P");

    let t = do_transition(
        SchedulerState::Waiting { graph },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("P".to_string()),
            outcome: NodeOutcome::PlanAccepted(PlanOutput {
                children: vec![
                    NodeRequest {
                        id: NodeId("child-1".to_string()),
                        kind: NodeKind::Work,
                        objective: "child one".to_string(),
                        target_files: vec![],
                        required_test_targets: vec![],
                        dependencies: vec![NodeId("P".to_string())],
                        validation_plan: None,
                    },
                    NodeRequest {
                        id: NodeId("child-2".to_string()),
                        kind: NodeKind::Work,
                        objective: "child two".to_string(),
                        target_files: vec![],
                        required_test_targets: vec![],
                        dependencies: vec![NodeId("P".to_string())],
                        validation_plan: None,
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
    assert!(t.effects.is_empty());
}

// ── Cancellation propagation tests ───────────────────────────────────────

#[test]
fn recovery_respects_graph_size_limit() {
    // When the graph is already at MAX_GRAPH_NODES, any recovery action must
    // fail the scheduler rather than inserting a replacement node.
    #[derive(Clone, Copy)]
    enum RecoveryKind {
        Retry,
        Split,
        Elevate,
    }

    for (case, recovery_kind) in [
        ("Retry", RecoveryKind::Retry),
        ("Split", RecoveryKind::Split),
        ("Elevate", RecoveryKind::Elevate),
    ] {
        let recovery = match recovery_kind {
            RecoveryKind::Retry => RecoveryAction::Retry {
                message: "try again".to_string(),
            },
            RecoveryKind::Split => RecoveryAction::Split {
                message: "decompose the work".to_string(),
            },
            RecoveryKind::Elevate => RecoveryAction::ElevateModel {
                message: "use stronger model".to_string(),
            },
        };

        let graph = graph_with_filler_nodes(work_node("W", "failing task", &[]), MAX_GRAPH_NODES);

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
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
        assert_eq!(graph.nodes.len(), MAX_GRAPH_NODES, "[{case}]");
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed, "[{case}]");
        assert!(
            graph.nodes.iter().all(|node| match recovery_kind {
                RecoveryKind::Retry => !matches!(node.origin, NodeOrigin::Retry { .. }),
                RecoveryKind::Split => !matches!(node.origin, NodeOrigin::Split { .. }),
                RecoveryKind::Elevate => !matches!(node.origin, NodeOrigin::ElevateModel { .. }),
            }),
            "[{case}] no replacement should be created"
        );
        assert!(
            reason.contains("graph size limit"),
            "[{case}] got: {reason:?}"
        );
        assert!(reason.contains(&MAX_GRAPH_NODES.to_string()), "[{case}]");
        assert!(t.effects.is_empty(), "[{case}]");
    }
}

// ── RecoverySummary / output classification tests ─────────────────────────

#[test]
fn split_depth_limit_fails_scheduler() {
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "complex task", &[])],
        next_id: 0,
    };
    graph.nodes[0].plan_depth = MAX_PLAN_DEPTH;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
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

    let SchedulerState::Failed { graph, reason } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 1, "must not insert split plan");
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert!(
        reason.contains("plan depth limit") && reason.contains(&MAX_PLAN_DEPTH.to_string()),
        "unexpected reason: {reason:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn no_ready_reports_missing_dependency() {
    // C is Pending and depends on B, but B does not exist in the graph.
    let graph = RunGraph {
        nodes: vec![work_node("C", "do C", &["B"])],
        next_id: 0,
    };
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);

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
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);

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
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);
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
    assert!(t.effects.is_empty());
}

#[test]
fn missing_dependency_fails_graph_validation() {
    let graph = RunGraph {
        nodes: vec![work_node("A", "do something", &["ghost"])],
        next_id: 0,
    };
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);
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
    assert!(t.effects.is_empty());
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
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);

    let SchedulerState::Waiting { graph } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };
    assert_eq!(active_node_id(&graph), Some(NodeId("root".to_string())));
    assert_eq!(graph.next_id, 0);
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::RunNode {
            node_id,
            ..
        }] if *node_id == NodeId("root".to_string())
    ));
}

#[test]
fn retry_origin_with_missing_source_fails_validation() {
    let mut node_b = work_node("B", "retry task", &[]);
    node_b.origin = NodeOrigin::Retry {
        source: NodeId("missing".to_string()),
    };
    let graph = RunGraph {
        nodes: vec![node_b],
        next_id: 0,
    };
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("missing origin source"),
        "reason should contain 'missing origin source', got: {reason:?}"
    );
    assert!(
        reason.contains("Retry"),
        "reason should mention Retry, got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain node id B, got: {reason:?}"
    );
    assert!(
        reason.contains("missing"),
        "reason should contain missing source id, got: {reason:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn elevate_origin_with_missing_source_fails_validation() {
    let mut node_b = work_node("B", "elevate task", &[]);
    node_b.origin = NodeOrigin::ElevateModel {
        source: NodeId("missing".to_string()),
    };
    let graph = RunGraph {
        nodes: vec![node_b],
        next_id: 0,
    };
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("missing origin source"),
        "reason should contain 'missing origin source', got: {reason:?}"
    );
    assert!(
        reason.contains("ElevateModel"),
        "reason should mention ElevateModel, got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain node id B, got: {reason:?}"
    );
    assert!(
        reason.contains("missing"),
        "reason should contain missing source id, got: {reason:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn split_origin_with_missing_source_fails_validation() {
    let mut node_b = work_node("B", "split task", &[]);
    node_b.origin = NodeOrigin::Split {
        source: NodeId("missing".to_string()),
    };
    let graph = RunGraph {
        nodes: vec![node_b],
        next_id: 0,
    };
    let t = do_transition(SchedulerState::Active { graph }, SchedulerEvent::Start);

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("missing origin source"),
        "reason should contain 'missing origin source', got: {reason:?}"
    );
    assert!(
        reason.contains("Split"),
        "reason should mention Split, got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain node id B, got: {reason:?}"
    );
    assert!(
        reason.contains("missing"),
        "reason should contain missing source id, got: {reason:?}"
    );
    assert!(t.effects.is_empty());
}

// ── Outcome/phase validation tests ───────────────────────────────────────
