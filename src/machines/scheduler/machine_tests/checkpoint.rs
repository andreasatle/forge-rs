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
            running: NodeId("P".to_string()),
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
    assert!(
        reason.contains("Plan"),
        "reason should mention Plan, got: {reason:?}"
    );
    assert!(
        reason.contains("WorkAccepted"),
        "reason should mention WorkAccepted, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn work_node_rejects_plan_accepted() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            running: NodeId("A".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("A".to_string()),
            outcome: NodeOutcome::PlanAccepted(PlanOutput { children: vec![] }),
        },
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("Work"),
        "reason should mention Work, got: {reason:?}"
    );
    assert!(
        reason.contains("PlanAccepted"),
        "reason should mention PlanAccepted, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn node_returned_rejects_integrating_node() {
    // Waiting { running: B } with B status = Integrating.
    // NodeReturned must be rejected: it is for the execution phase only.
    let mut graph = RunGraph {
        nodes: vec![work_node("B", "do work", &[])],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("B".to_string()),
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
    assert!(
        reason.contains("protocol violation"),
        "reason should contain 'protocol violation', got: {reason:?}"
    );
    assert!(
        reason.contains("NodeReturned"),
        "reason should contain 'NodeReturned', got: {reason:?}"
    );
    assert!(
        reason.contains("Running"),
        "reason should mention expected status Running, got: {reason:?}"
    );
    assert!(
        reason.contains("Integrating"),
        "reason should mention actual status Integrating, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn integration_returned_rejects_non_integrating_work() {
    // Work node is Running (not Integrating) when IntegrationReturned arrives.
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            running: NodeId("A".to_string()),
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
    assert!(
        reason.contains("Integrating"),
        "reason should mention Integrating, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
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
            running: NodeId("P".to_string()),
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
    assert!(
        reason.contains("Work") || reason.contains("Plan"),
        "reason should mention Work or Plan, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
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
            running: NodeId("A".to_string()),
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
    assert!(
        reason.contains("protocol violation"),
        "reason should contain 'protocol violation', got: {reason:?}"
    );
    assert!(
        reason.contains('A'),
        "reason should contain expected node A, got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain received node B, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
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
            running: NodeId("A".to_string()),
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
    assert!(
        reason.contains("protocol violation"),
        "reason should contain 'protocol violation', got: {reason:?}"
    );
    assert!(
        reason.contains('A'),
        "reason should contain expected node A, got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain received node B, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn running_rejects_node_returned() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Running { graph },
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
    assert!(
        reason.contains("protocol violation"),
        "reason should contain 'protocol violation', got: {reason:?}"
    );
    assert!(
        reason.contains("Running"),
        "reason should mention Running, got: {reason:?}"
    );
    assert!(
        reason.contains("NodeReturned"),
        "reason should mention NodeReturned, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn running_rejects_integration_returned() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Running { graph },
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
    assert!(
        reason.contains("protocol violation"),
        "reason should contain 'protocol violation', got: {reason:?}"
    );
    assert!(
        reason.contains("Running"),
        "reason should mention Running, got: {reason:?}"
    );
    assert!(
        reason.contains("IntegrationReturned"),
        "reason should mention IntegrationReturned, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn waiting_rejects_start() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("A".to_string()),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("protocol violation"),
        "reason should contain 'protocol violation', got: {reason:?}"
    );
    assert!(
        reason.contains("Waiting"),
        "reason should mention Waiting, got: {reason:?}"
    );
    assert!(
        reason.contains("Start"),
        "reason should mention Start, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

// ── Waiting-state invariant validation tests ──────────────────────────────

#[test]
fn waiting_with_missing_running_node_fails() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("missing".to_string()),
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
    assert!(
        reason.contains("invalid waiting state"),
        "reason should contain 'invalid waiting state', got: {reason:?}"
    );
    assert!(
        reason.contains("missing"),
        "reason should contain the missing node id, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn waiting_with_completed_running_node_fails() {
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
            running: NodeId("B".to_string()),
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
    assert!(
        reason.contains("invalid waiting state"),
        "reason should contain 'invalid waiting state', got: {reason:?}"
    );
    assert!(
        reason.contains("Completed"),
        "reason should contain the actual status, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn waiting_with_running_node_still_works() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            running: NodeId("A".to_string()),
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
fn running_state_rejects_preexisting_active_node() {
    let mut graph = single_work_graph();
    graph.nodes[0].status = NodeStatus::Running;

    let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("invalid running state"),
        "reason should contain 'invalid running state', got: {reason:?}"
    );
    assert!(
        reason.contains('A'),
        "reason should contain the node id, got: {reason:?}"
    );
    assert!(
        reason.contains("Running"),
        "reason should contain the status, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn running_state_rejects_preexisting_integrating_node() {
    let mut graph = single_work_graph();
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(SchedulerState::Running { graph }, SchedulerEvent::Start);

    let SchedulerState::Failed { reason, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert!(
        reason.contains("invalid running state"),
        "reason should contain 'invalid running state', got: {reason:?}"
    );
    assert!(
        reason.contains('A'),
        "reason should contain the node id, got: {reason:?}"
    );
    assert!(
        reason.contains("Integrating"),
        "reason should contain the status, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
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
            running: NodeId("B".to_string()),
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
    assert!(
        reason.contains("invalid waiting state"),
        "reason should contain 'invalid waiting state', got: {reason:?}"
    );
    assert!(
        reason.contains("found none") || reason.contains("Pending"),
        "reason should mention 'found none' or equivalent status, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
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
            running: NodeId("B".to_string()),
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
    assert!(
        reason.contains("multiple active nodes"),
        "reason should contain 'multiple active nodes', got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain node id B, got: {reason:?}"
    );
    assert!(
        reason.contains('C'),
        "reason should contain node id C, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn waiting_state_rejects_active_node_that_differs_from_running() {
    // C is active, B is non-active — the active node doesn't match waiting.running.
    let mut graph = RunGraph {
        nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
        next_id: 0,
    };
    graph.nodes[1].status = NodeStatus::Running;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("B".to_string()),
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
    assert!(
        reason.contains("invalid waiting state"),
        "reason should contain 'invalid waiting state', got: {reason:?}"
    );
    assert!(
        reason.contains('B'),
        "reason should contain waiting.running id B, got: {reason:?}"
    );
    assert!(
        reason.contains('C'),
        "reason should contain active node id C, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

// ── Model-tier policy tests ───────────────────────────────────────────────
