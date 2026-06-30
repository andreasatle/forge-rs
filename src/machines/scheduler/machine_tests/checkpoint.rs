use super::*;

#[test]
fn plan_node_rejects_work_accepted() {
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("P".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "work done".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Plan"),
        "detail should mention Plan, got: {detail:?}"
    );
    assert!(
        detail.contains("WorkAccepted"),
        "detail should mention WorkAccepted, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn work_node_rejects_plan_accepted() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("A".to_string()),
            outcome: NodeOutcome::PlanAccepted(PlanOutput { children: vec![] }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Work"),
        "detail should mention Work, got: {detail:?}"
    );
    assert!(
        detail.contains("PlanAccepted"),
        "detail should mention PlanAccepted, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn node_returned_rejects_integrating_node() {
    // Waiting with B status = Integrating.
    // NodeReturned must be rejected: it is for the execution phase only.
    let mut graph = RunGraph {
        nodes: vec![work_node("B", "do work", &[])],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "spurious result".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("NodeReturned"),
        "detail should contain 'NodeReturned', got: {detail:?}"
    );
    assert!(
        detail.contains("Running"),
        "detail should mention expected status Running, got: {detail:?}"
    );
    assert!(
        detail.contains("Integrating"),
        "detail should mention actual status Integrating, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn integration_returned_rejects_non_integrating_work() {
    // Work node is Running (not Integrating) when IntegrationReturned arrives.
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("A".to_string()),
            outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                summary: "done".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Integrating"),
        "detail should mention Integrating, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn integration_returned_rejects_plan_node() {
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("P".to_string()),
            outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                summary: "done".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Work") || detail.contains("Plan"),
        "detail should mention Work or Plan, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

// ── Protocol violation tests ──────────────────────────────────────────────

#[test]
fn node_returned_wrong_node_fails_scheduler() {
    let graph = RunGraph {
        nodes: vec![work_node("A", "task A", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "spurious result".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains('A'),
        "detail should contain expected node A, got: {detail:?}"
    );
    assert!(
        detail.contains('B'),
        "detail should contain received node B, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn integration_returned_wrong_node_fails_scheduler() {
    let mut graph = RunGraph {
        nodes: vec![work_node("A", "task A", &[])],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("B".to_string()),
            outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                summary: "spurious result".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains('A'),
        "detail should contain expected node A, got: {detail:?}"
    );
    assert!(
        detail.contains('B'),
        "detail should contain received node B, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn active_rejects_node_returned() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("A".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "spurious".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Active"),
        "detail should mention Active, got: {detail:?}"
    );
    assert!(
        detail.contains("NodeReturned"),
        "detail should mention NodeReturned, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn active_rejects_integration_returned() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("A".to_string()),
            outcome: IntegrationOutcome::Succeeded(IntegrationOutput {
                summary: "spurious".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Active"),
        "detail should mention Active, got: {detail:?}"
    );
    assert!(
        detail.contains("IntegrationReturned"),
        "detail should mention IntegrationReturned, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn waiting_rejects_start() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("Waiting"),
        "detail should mention Waiting, got: {detail:?}"
    );
    assert!(
        detail.contains("Start"),
        "detail should mention Start, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

// ── Waiting-state invariant validation tests ──────────────────────────────

#[test]
fn waiting_with_no_active_node_fails_before_matching_returned_node() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("missing".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "irrelevant".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("invalid waiting state"),
        "detail should contain 'invalid waiting state', got: {detail:?}"
    );
    assert!(
        detail.contains("found none"),
        "detail should mention that no active node exists, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn waiting_with_only_completed_nodes_fails() {
    let mut graph = RunGraph {
        nodes: vec![
            work_node("A", "done", &[]),
            work_node("B", "also done", &["A"]),
        ],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Completed;
    graph.nodes[1].status = NodeStatus::Completed;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "irrelevant".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("invalid waiting state"),
        "detail should contain 'invalid waiting state', got: {detail:?}"
    );
    assert!(
        detail.contains("found none"),
        "detail should mention that no active node exists, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn waiting_with_running_node_still_works() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("A".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "success".to_string(),
            }),
        },
    );

    assert!(
        matches!(t.state, SchedulerState::Waiting { .. }),
        "expected Waiting (Integrating), got {:#?}",
        t.state
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::IntegrateWork { .. }]
    ));
}

// ── Serial active-node invariant tests ───────────────────────────────────

#[test]
fn active_state_rejects_preexisting_active_node() {
    let mut graph = single_work_graph();
    graph.nodes[0].status = NodeStatus::Running;

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
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("invalid running state"),
        "detail should contain 'invalid running state', got: {detail:?}"
    );
    assert!(
        detail.contains('A'),
        "detail should contain the node id, got: {detail:?}"
    );
    assert!(
        detail.contains("Running"),
        "detail should contain the status, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn active_state_rejects_preexisting_integrating_node() {
    let mut graph = single_work_graph();
    graph.nodes[0].status = NodeStatus::Integrating;

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
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("invalid running state"),
        "detail should contain 'invalid running state', got: {detail:?}"
    );
    assert!(
        detail.contains('A'),
        "detail should contain the node id, got: {detail:?}"
    );
    assert!(
        detail.contains("Integrating"),
        "detail should contain the status, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn waiting_state_rejects_no_active_nodes() {
    // B exists but is Pending — no active node in the graph.
    let graph = RunGraph {
        nodes: vec![work_node("B", "do B", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "irrelevant".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("invalid waiting state"),
        "detail should contain 'invalid waiting state', got: {detail:?}"
    );
    assert!(
        detail.contains("found none") || detail.contains("Pending"),
        "detail should mention 'found none' or equivalent status, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn waiting_state_rejects_multiple_active_nodes() {
    // B and C are both active — violates the serial invariant.
    let mut graph = RunGraph {
        nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Running;
    graph.nodes[1].status = NodeStatus::Running;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "irrelevant".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains("multiple active nodes"),
        "detail should contain 'multiple active nodes', got: {detail:?}"
    );
    assert!(
        detail.contains('B'),
        "detail should contain node id B, got: {detail:?}"
    );
    assert!(
        detail.contains('C'),
        "detail should contain node id C, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn waiting_state_rejects_return_for_non_active_node() {
    // C is active; a return for B is a protocol violation.
    let mut graph = RunGraph {
        nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
        next_id: 0,
    };
    graph.nodes[1].status = NodeStatus::Running;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::WorkAccepted(WorkOutput {
                summary: "irrelevant".to_string(),
            }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert!(
        detail.contains('B'),
        "detail should contain returned node id B, got: {detail:?}"
    );
    assert!(
        detail.contains('C'),
        "detail should contain active node id C, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

// ── Model-tier policy tests ───────────────────────────────────────────────
