use super::*;

#[test]
fn run_request_starts_scheduler_end_to_end() {
    let request = RunRequest {
        objective: "plan demo".to_string(),
    };
    let state = SchedulerMachine::initial_state(request, RunConfig::default());
    let output = run_machine(scheduler_handler(), state);
    assert!(matches!(output, SchedulerOutput::Complete { .. }));
}

// ── Active + Start structural tests ──────────────────────────────────────

#[test]
fn active_start_all_complete_moves_to_complete() {
    let mut graph = single_work_graph();
    graph.nodes[0].status = NodeStatus::Completed;
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    assert!(matches!(t.state, SchedulerState::Complete { .. }));
    assert!(t.effects.is_empty());
}

#[test]
fn final_run_fails_when_required_test_target_is_absent() {
    let graph = RunGraph {
        nodes: vec![Node {
            target_files: vec!["main.py".to_string()],
            required_test_targets: vec!["test_main.py".to_string()],
            status: NodeStatus::Completed,
            ..work_node("source", "implement fibonacci", &[])
        }],
        next_id: 0,
    };

    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::RequiredTestTargetsMissing(detail) = reason else {
        panic!("expected RequiredTestTargetsMissing, got {reason:?}");
    };
    assert!(
        detail.contains("test_main.py"),
        "failure reason should identify missing required test target; got: {detail}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn final_run_completes_when_required_test_target_completed_separately() {
    let graph = RunGraph {
        nodes: vec![
            Node {
                target_files: vec!["main.py".to_string()],
                required_test_targets: vec!["test_main.py".to_string()],
                status: NodeStatus::Completed,
                ..work_node("source", "implement fibonacci", &[])
            },
            Node {
                target_files: vec!["test_main.py".to_string()],
                status: NodeStatus::Completed,
                ..work_node("tests", "write tests", &["source"])
            },
        ],
        next_id: 0,
    };

    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    assert!(matches!(t.state, SchedulerState::Complete { .. }));
}

#[test]
fn active_start_no_ready_moves_to_failed() {
    let graph = RunGraph {
        nodes: vec![work_node("B", "blocked", &["A"])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    assert!(matches!(t.state, SchedulerState::Failed { .. }));
    assert!(t.effects.is_empty());
}

#[test]
fn active_start_dispatches_ready_node_and_waits() {
    let t = do_transition(
        SchedulerState::Active {
            graph: single_work_graph(),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting")
    };
    assert_eq!(active_node_id(&graph), Some(NodeId("A".to_string())));
    assert_eq!(graph.nodes[0].status, NodeStatus::Running);
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::RunNode { .. }]
    ));
}

// ── new outcome tests ──────────────────────────────────────────────────────

#[test]
fn terminal_failure_produces_failed_scheduler_output() {
    let graph = RunGraph {
        nodes: vec![Node {
            id: NodeId("T".to_string()),
            kind: NodeKind::Work,
            objective: "fail this step".to_string(),
            target_files: vec![],
            required_test_targets: vec![],
            dependencies: vec![],
            status: NodeStatus::Pending,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
            retry_feedback: None,
        }],
        next_id: 0,
    };
    let output = run_machine(
        scheduler_handler(),
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
    );
    assert!(matches!(output, SchedulerOutput::Failed { .. }));
}

#[test]
fn scheduler_output_includes_node_failure_reason() {
    let graph = RunGraph {
        nodes: vec![Node {
            id: NodeId("T".to_string()),
            kind: NodeKind::Work,
            objective: "fail this step".to_string(),
            target_files: vec![],
            required_test_targets: vec![],
            dependencies: vec![],
            status: NodeStatus::Running,
            attempt: 0,
            plan_depth: 0,
            model_tier: ModelTier::Cheap,
            summary: None,
            origin: NodeOrigin::Root,
            validation_plan: None,
            retry_feedback: None,
        }],
        next_id: 0,
    };

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("T".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "provider error (Retryable): connection refused".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "deliberation failed".to_string(),
                },
            }),
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
    assert_eq!(terminal_message, "deliberation failed");
    assert!(failure_message.contains("provider error (Retryable): connection refused"));
}

#[test]
fn three_node_chain_completes_via_handler() {
    let graph = RunGraph {
        nodes: vec![
            work_node("A", "step A", &[]),
            work_node("B", "step B", &["A"]),
            work_node("C", "step C", &["B"]),
        ],
        next_id: 0,
    };

    let output = run_machine(
        scheduler_handler(),
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
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
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching A")
    };

    // A completes: WorkAccepted → Integrating.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("A".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "A done".to_string(),
            }),
        },
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after A WorkAccepted")
    };

    // Integration succeeds → Active.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("A".to_string()),
            outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                summary: "A integrated".to_string(),
            }),
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after A integrates")
    };

    // Dispatch B.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching B")
    };

    // B fails with Split.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "task too complex".to_string(),
                recovery: RecoveryAction::Split {
                    message: "decompose the work".to_string(),
                },
            }),
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after Split")
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
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching P")
    };

    // P completes as a Plan with no children.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: split_id.clone(),
            outcome: NodeOutcome::PlanAccepted(PlanOutput { children: vec![] }),
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after P completes")
    };

    // Dispatch C (now ready: P is Completed and C depends on P).
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching C")
    };

    // C completes: WorkAccepted → Integrating.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("C".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "C done".to_string(),
            }),
        },
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after C WorkAccepted")
    };

    // Integration succeeds → Active.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("C".to_string()),
            outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                summary: "C integrated".to_string(),
            }),
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after C integrates")
    };

    // All nodes terminal → scheduler reaches Complete.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Complete { graph, .. } = t.state else {
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
    let output = run_machine(
        scheduler_handler(),
        SchedulerState::Active {
            graph: chain_graph(),
            run_config: RunConfig::default(),
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
fn clean_success_has_no_recovery() {
    let output = run_machine(
        scheduler_handler(),
        SchedulerState::Active {
            graph: single_work_graph(),
            run_config: RunConfig::default(),
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
                target_files: vec![],
                required_test_targets: vec![],
                dependencies: vec![],
                status: NodeStatus::Failed,
                attempt: 0,
                plan_depth: 0,
                model_tier: ModelTier::Cheap,
                summary: None,
                origin: NodeOrigin::Root,
                validation_plan: None,
                retry_feedback: None,
            },
            Node {
                id: split_id,
                kind: NodeKind::Plan,
                objective: "decompose complex task".to_string(),
                target_files: vec![],
                required_test_targets: vec![],
                dependencies: vec![],
                status: NodeStatus::Completed,
                attempt: 1,
                plan_depth: 1,
                model_tier: ModelTier::Strong,
                summary: Some("planned".to_string()),
                origin: NodeOrigin::Split { source: source_id },
                validation_plan: None,
                retry_feedback: None,
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
fn event_at_terminal_state_returns_protocol_violation() {
    // Invariant: transition is total; events delivered to Complete or Failed
    // must not panic but must return ProtocolViolation instead.
    let mut terminal_node = work_node("A", "done", &[]);
    terminal_node.status = NodeStatus::Completed;
    let terminal_graph = RunGraph {
        nodes: vec![terminal_node],
        next_id: 0,
    };
    for (label, state) in [
        (
            "Complete",
            SchedulerState::Complete {
                graph: terminal_graph.clone(),
            },
        ),
        (
            "Failed",
            SchedulerState::Failed {
                graph: terminal_graph.clone(),
                reason: FailureReason::ProtocolViolation("prior failure".to_string()),
            },
        ),
    ] {
        let t = do_transition(state, SchedulerEvent::Start);
        let SchedulerState::Failed { reason, .. } = t.state else {
            panic!("[{label}] expected Failed, got {:#?}", t.state);
        };
        assert!(
            matches!(reason, FailureReason::ProtocolViolation(_)),
            "[{label}] expected ProtocolViolation, got {reason:?}"
        );
        assert!(t.effects.is_empty(), "[{label}]");
    }
}
