use super::*;

fn assert_protocol_violation(t: &Transition<SchedulerState, SchedulerEffect>, expected: &str) {
    let SchedulerState::Failed { reason, .. } = &t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    let FailureReason::ProtocolViolation(detail) = reason else {
        panic!("expected ProtocolViolation, got {reason:?}");
    };
    assert_eq!(detail, expected);
    assert!(t.effects.is_empty());
}

// ── Event/node-kind mismatch tests ────────────────────────────────────────
//
// Invariant: an event whose payload kind doesn't match the target node's
// kind or status is always rejected as a ProtocolViolation with no effects,
// regardless of which specific mismatch triggered it.

#[test]
fn event_kind_or_status_mismatch_fails_with_protocol_violation() {
    struct Case {
        graph: RunGraph,
        event: SchedulerEvent,
        expected_detail: &'static str,
    }

    let cases = vec![
        // Plan node receiving a WorkAccepted outcome.
        Case {
            graph: running(
                RunGraph {
                    nodes: vec![plan_node("P", "plan something", &[])],
                },
                "P",
            ),
            event: SchedulerEvent::WorkAccepted {
                node_id: NodeId("P".to_string()),
                work: WorkOutput {
                    summary: "work done".to_string(),
                },
            },
            expected_detail: "node P is Plan but received WorkAccepted outcome",
        },
        // Work node receiving a PlanAccepted outcome.
        Case {
            graph: running(single_work_graph(), "A"),
            event: SchedulerEvent::PlanAccepted {
                node_id: NodeId("A".to_string()),
                plan: PlanOutput {
                    children: vec![],
                    tasks: vec![],
                },
            },
            expected_detail: "node A is Work but received PlanAccepted outcome",
        },
        // NodeReturned event while the node is Integrating, not Running.
        Case {
            graph: {
                let mut g = RunGraph {
                    nodes: vec![work_node("B", "do work", &[])],
                };
                g.nodes[0].status = NodeStatus::Integrating;
                g
            },
            event: SchedulerEvent::WorkAccepted {
                node_id: NodeId("B".to_string()),
                work: WorkOutput {
                    summary: "spurious result".to_string(),
                },
            },
            expected_detail: "protocol violation: NodeReturned for node B expected Running but found Integrating",
        },
        // IntegrationSucceeded while the work node is Running, not Integrating.
        Case {
            graph: running(single_work_graph(), "A"),
            event: SchedulerEvent::IntegrationSucceeded {
                node_id: NodeId("A".to_string()),
                output: IntegrationOutput {
                    summary: "done".to_string(),
                },
                manifest_tasks: vec![],
            },
            expected_detail: "node A has status Running but IntegrationReturned requires Integrating",
        },
        // IntegrationSucceeded for a Plan node instead of a Work node.
        Case {
            graph: running(
                RunGraph {
                    nodes: vec![plan_node("P", "plan something", &[])],
                },
                "P",
            ),
            event: SchedulerEvent::IntegrationSucceeded {
                node_id: NodeId("P".to_string()),
                output: IntegrationOutput {
                    summary: "done".to_string(),
                },
                manifest_tasks: vec![],
            },
            expected_detail: "node P is Plan but IntegrationReturned requires a Work node",
        },
    ];

    for case in cases {
        let t = do_transition(
            SchedulerState::Waiting {
                graph: case.graph,
                run_config: RunConfig::default(),
            },
            case.event,
        );
        assert_protocol_violation(&t, case.expected_detail);
    }
}

// ── Wrong-node-id tests ────────────────────────────────────────────────────
//
// Invariant: a return event for a node id other than the single active node
// is a ProtocolViolation naming both the expected and received ids, whether
// or not the received id even exists in the graph.

#[test]
fn wrong_node_id_fails_scheduler_with_protocol_violation() {
    struct Case {
        graph: RunGraph,
        event: SchedulerEvent,
        expected_detail: &'static str,
    }

    let cases = vec![
        // WorkAccepted for an id absent from the graph.
        Case {
            graph: running(
                RunGraph {
                    nodes: vec![work_node("A", "task A", &[])],
                },
                "A",
            ),
            event: SchedulerEvent::WorkAccepted {
                node_id: NodeId("B".to_string()),
                work: WorkOutput {
                    summary: "spurious result".to_string(),
                },
            },
            expected_detail: "expected result for node A but received B",
        },
        // IntegrationSucceeded for an id absent from the graph.
        Case {
            graph: {
                let mut g = RunGraph {
                    nodes: vec![work_node("A", "task A", &[])],
                };
                g.nodes[0].status = NodeStatus::Integrating;
                g
            },
            event: SchedulerEvent::IntegrationSucceeded {
                node_id: NodeId("B".to_string()),
                output: IntegrationOutput {
                    summary: "spurious result".to_string(),
                },
                manifest_tasks: vec![],
            },
            expected_detail: "expected integration result for node A but received B",
        },
        // WorkAccepted for an id that exists in the graph but isn't active.
        Case {
            graph: {
                let mut g = RunGraph {
                    nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
                };
                g.nodes[1].status = NodeStatus::Running;
                g
            },
            event: SchedulerEvent::WorkAccepted {
                node_id: NodeId("B".to_string()),
                work: WorkOutput {
                    summary: "irrelevant".to_string(),
                },
            },
            expected_detail: "expected result for node C but received B",
        },
    ];

    for case in cases {
        let t = do_transition(
            SchedulerState::Waiting {
                graph: case.graph,
                run_config: RunConfig::default(),
            },
            case.event,
        );
        assert_protocol_violation(&t, case.expected_detail);
    }
}

// ── Active-state-rejects-return-events tests ───────────────────────────────
//
// Invariant: the Active state only ever consumes Start; any return-type
// event (node or integration) is a ProtocolViolation naming the event kind.

#[test]
fn active_state_rejects_return_events() {
    struct Case {
        event: SchedulerEvent,
        expected_detail: &'static str,
    }

    let cases = vec![
        Case {
            event: SchedulerEvent::WorkAccepted {
                node_id: NodeId("A".to_string()),
                work: WorkOutput {
                    summary: "spurious".to_string(),
                },
            },
            expected_detail: "state Active cannot consume NodeReturned",
        },
        Case {
            event: SchedulerEvent::IntegrationSucceeded {
                node_id: NodeId("A".to_string()),
                output: IntegrationOutput {
                    summary: "spurious".to_string(),
                },
                manifest_tasks: vec![],
            },
            expected_detail: "state Active cannot consume IntegrationReturned",
        },
    ];

    for case in cases {
        let t = do_transition(
            SchedulerState::Active {
                graph: single_work_graph(),
                run_config: RunConfig::default(),
            },
            case.event,
        );
        assert_protocol_violation(&t, case.expected_detail);
    }
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
//
// Invariant: the Waiting state requires exactly one active node before it
// will match a return event against it; zero active nodes is always a
// ProtocolViolation with the same "found none" detail, regardless of why
// there are no active nodes (never started, or all already completed).

#[test]
fn waiting_with_no_active_node_fails_with_protocol_violation() {
    struct Case {
        graph: RunGraph,
    }

    let cases = vec![
        // No node has ever been marked Running.
        Case {
            graph: single_work_graph(),
        },
        // Both nodes have already completed.
        Case {
            graph: {
                let mut g = RunGraph {
                    nodes: vec![
                        work_node("A", "done", &[]),
                        work_node("B", "also done", &["A"]),
                    ],
                };
                g.nodes[0].status = NodeStatus::Completed;
                g.nodes[1].status = NodeStatus::Completed;
                g
            },
        },
        // The only node in the graph is still Pending.
        Case {
            graph: RunGraph {
                nodes: vec![work_node("B", "do B", &[])],
            },
        },
    ];

    for case in cases {
        let t = do_transition(
            SchedulerState::Waiting {
                graph: case.graph,
                run_config: RunConfig::default(),
            },
            SchedulerEvent::WorkAccepted {
                node_id: NodeId("B".to_string()),
                work: WorkOutput {
                    summary: "irrelevant".to_string(),
                },
            },
        );

        assert_protocol_violation(
            &t,
            "invalid waiting state: expected exactly one active node; found none",
        );
    }
}

#[test]
fn waiting_with_running_node_still_works() {
    let graph = single_work_graph();
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "A"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("A".to_string()),
            work: WorkOutput {
                summary: "success".to_string(),
            },
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
//
// Invariant: Start requires that no node is already Running or Integrating;
// either pre-existing status is rejected as a ProtocolViolation naming the
// node id and its actual status.

#[test]
fn active_state_rejects_preexisting_active_node() {
    struct Case {
        status: NodeStatus,
        expected_detail: &'static str,
    }

    let cases = vec![
        Case {
            status: NodeStatus::Running,
            expected_detail: "invalid running state: node A is Running",
        },
        Case {
            status: NodeStatus::Integrating,
            expected_detail: "invalid running state: node A is Integrating",
        },
    ];

    for case in cases {
        let mut graph = single_work_graph();
        graph.nodes[0].status = case.status;

        let t = do_transition(
            SchedulerState::Active {
                graph,
                run_config: RunConfig::default(),
            },
            SchedulerEvent::Start,
        );

        assert_protocol_violation(&t, case.expected_detail);
    }
}

#[test]
fn waiting_state_rejects_multiple_active_nodes() {
    // B and C are both active — violates the serial invariant.
    let mut graph = RunGraph {
        nodes: vec![work_node("B", "do B", &[]), work_node("C", "do C", &[])],
    };
    graph.nodes[0].status = NodeStatus::Running;
    graph.nodes[1].status = NodeStatus::Running;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("B".to_string()),
            work: WorkOutput {
                summary: "irrelevant".to_string(),
            },
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

// ── Model-tier policy tests ───────────────────────────────────────────────
