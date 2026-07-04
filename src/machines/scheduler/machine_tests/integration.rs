use super::*;

#[test]
fn work_node_accepted_marks_integrating_and_emits_integrate_work() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("A".to_string()),
            work: WorkOutput {
                summary: "done!".to_string(),
            },
        },
    );

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting")
    };
    assert_eq!(active_node_id(&graph), Some(NodeId("A".to_string())));
    assert_ne!(
        graph.nodes[0].status,
        NodeStatus::Completed,
        "WorkAccepted must not complete the node"
    );
    assert_eq!(graph.nodes[0].status, NodeStatus::Integrating);
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::IntegrateWork { .. }]
    ));
}

#[test]
fn scheduler_terminal_output_includes_integration_failure_reason() {
    let graph = RunGraph {
        nodes: vec![Node {
            id: NodeId("W".to_string()),
            kind: NodeKind::Work,
            worker_role: None,
            objective: "integrate this step".to_string(),
            target_files: vec![],
            required_validation_targets: vec![],
            dependencies: vec![],
            status: NodeStatus::Integrating,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
            retry_feedback: None,
        }],
        next_id: 0,
        id_seed: 0,
    };

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationFailed {
            node_id: NodeId("W".to_string()),
            failure: IntegrationFailure {
                kind: FailureKind::IntegrationFailure,
                message: "validation failed: cargo test failed".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "integration failed".to_string(),
                },
            },
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::TerminalRecovery {
        terminal_message,
        failure_message,
    } = reason
    else {
        panic!("expected TerminalRecovery, got {reason:?}");
    };
    assert_eq!(terminal_message, "integration failed");
    assert!(failure_message.contains("validation failed: cargo test failed"));
}

#[test]
fn integration_failure_routes_to_recovery_replacement() {
    // Graph: A -> B -> C; B is Integrating (work accepted, integration pending).
    // Invariant: whichever recovery action fires on IntegrationFailed, the
    // original B becomes Failed, exactly one replacement is inserted with a
    // fresh id, the recovery-specific kind/tier/attempt and origin source B,
    // C's dependency is remapped from B to the replacement, and the
    // scheduler returns to Active with no effects.
    struct Case {
        recovery: RecoveryAction,
        initial_attempt: u32,
        expected_kind: NodeKind,
        expected_tier: ModelTier,
    }

    let cases = vec![
        Case {
            recovery: RecoveryAction::Retry {
                message: "retry after integration failure".to_string(),
            },
            initial_attempt: 0,
            expected_kind: NodeKind::Work,
            expected_tier: ModelTier::Cheap,
        },
        Case {
            recovery: RecoveryAction::ElevateModel {
                message: "use stronger model".to_string(),
            },
            initial_attempt: 1,
            expected_kind: NodeKind::Work,
            expected_tier: ModelTier::Strong,
        },
        Case {
            recovery: RecoveryAction::Split {
                message: "decompose step B".to_string(),
            },
            initial_attempt: 0,
            expected_kind: NodeKind::Plan,
            expected_tier: ModelTier::Strong,
        },
    ];

    for case in cases {
        let mut graph = RunGraph {
            nodes: vec![
                work_node("A", "step A", &[]),
                work_node("B", "step B", &["A"]),
                work_node("C", "step C", &["B"]),
            ],
            next_id: 0,
            id_seed: 0,
        };
        graph.nodes[0].status = NodeStatus::Completed;
        graph.nodes[1].status = NodeStatus::Integrating;
        graph.nodes[1].attempt = case.initial_attempt;

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                run_config: RunConfig::default(),
            },
            SchedulerEvent::IntegrationFailed {
                node_id: NodeId("B".to_string()),
                failure: IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "integration error".to_string(),
                    recovery: case.recovery,
                },
            },
        );

        let SchedulerState::Active { graph, .. } = t.state else {
            panic!("expected Active, got {:#?}", t.state);
        };

        let b = graph.nodes.iter().find(|n| n.id.0 == "B").expect("B");
        assert_eq!(b.status, NodeStatus::Failed);

        // Found by its origin source rather than by parsing the id.
        let replacement = graph
            .nodes
            .iter()
            .find(|n| {
                matches!(
                    &n.origin,
                    NodeOrigin::Retry { source }
                    | NodeOrigin::ElevateModel { source }
                    | NodeOrigin::Split { source }
                        if source.0 == "B"
                )
            })
            .expect("no replacement with origin source B");
        assert_eq!(replacement.kind, case.expected_kind);
        assert_eq!(replacement.model_tier, case.expected_tier);
        assert_eq!(replacement.attempt, case.initial_attempt + 1);
        assert_eq!(replacement.status, NodeStatus::Pending);

        let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
        assert!(
            !c.dependencies.contains(&NodeId("B".to_string())),
            "C still depends on failed B"
        );
        assert!(
            c.dependencies.contains(&replacement.id),
            "C does not depend on the replacement"
        );

        assert!(t.effects.is_empty());
    }
}
