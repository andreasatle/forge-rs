use super::*;

#[test]
fn work_node_accepted_marks_integrating_and_emits_integrate_work() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("A".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "done!".to_string(),
            }),
        },
    );

    let SchedulerState::Waiting { graph } = t.state else {
        panic!("expected Waiting")
    };
    assert_eq!(active_node_id(&graph), Some(NodeId("A".to_string())));
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
        },
        SchedulerEvent::NodeReturned {
            node_id: node_id.clone(),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "work done".to_string(),
            }),
        },
    );

    let SchedulerState::Waiting { ref graph } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };
    assert_eq!(active_node_id(graph), Some(node_id.clone()));
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
fn scheduler_output_includes_integration_failure_reason() {
    let graph = RunGraph {
        nodes: vec![Node {
            id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            objective: "integrate this step".to_string(),
            target_files: vec![],
            required_test_targets: vec![],
            dependencies: vec![],
            status: NodeStatus::Integrating,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
        }],
        next_id: 0,
    };

    let t = do_transition(
        SchedulerState::Waiting { graph },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("W".to_string()),
            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                kind: FailureKind::IntegrationFailure,
                message: "validation failed: cargo test failed".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "integration failed".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(reason.contains("integration failed"));
    assert!(reason.contains("validation failed: cargo test failed"));
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
        SchedulerState::Waiting { graph },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("B".to_string()),
            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                kind: FailureKind::IntegrationFailure,
                message: "integration error".to_string(),
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
        SchedulerState::Waiting { graph },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("B".to_string()),
            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                kind: FailureKind::IntegrationFailure,
                message: "integration error".to_string(),
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
        SchedulerState::Waiting { graph },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("B".to_string()),
            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                kind: FailureKind::IntegrationFailure,
                message: "integration error".to_string(),
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
