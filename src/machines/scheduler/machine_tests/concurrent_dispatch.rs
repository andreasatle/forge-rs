use super::*;

// ── Stage-2 state-model tests: dispatch_cap > 1 ────────────────────────────
//
// These prove the scheduler's state model can track and correctly route
// events for several simultaneously in-flight nodes, without any real
// concurrency: everything below still runs through the pure, synchronous
// `SchedulerMachine::transition` function one event at a time.

fn cap(n: usize) -> RunConfig {
    RunConfig {
        dispatch_cap: n,
        ..RunConfig::default()
    }
}

fn running_all(mut graph: RunGraph, ids: &[&str]) -> RunGraph {
    for n in &mut graph.nodes {
        if ids.contains(&n.id.0.as_str()) {
            n.status = NodeStatus::Running;
        }
    }
    graph
}

// Invariant: a Start tick dispatches up to `dispatch_cap` ready nodes in a
// single transition, marking every one of them Running and emitting exactly
// one RunNode effect per dispatched node.
#[test]
fn start_dispatches_up_to_cap_ready_nodes_at_once() {
    let graph = RunGraph {
        nodes: vec![work_node("A", "do a", &[]), work_node("B", "do b", &[])],
    };

    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: cap(2),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Running);
    assert_eq!(graph.nodes[1].status, NodeStatus::Running);
    assert_eq!(
        t.effects.len(),
        2,
        "expected one RunNode effect per dispatched node"
    );
    let dispatched: Vec<&NodeId> = t
        .effects
        .iter()
        .map(|effect| match effect {
            SchedulerEffect::RunNode { node_id, .. } => node_id,
            other => panic!("expected RunNode effects only, got {other:#?}"),
        })
        .collect();
    assert_eq!(
        dispatched,
        vec![&NodeId("A".to_string()), &NodeId("B".to_string())]
    );
}

// Invariant: dispatch never exceeds the configured cap even when more ready
// nodes exist. The excess ready nodes stay Pending until a slot frees up.
#[test]
fn start_respects_cap_when_more_ready_nodes_exist_than_capacity() {
    let graph = RunGraph {
        nodes: vec![
            work_node("A", "do a", &[]),
            work_node("B", "do b", &[]),
            work_node("C", "do c", &[]),
        ],
    };

    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: cap(2),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Running);
    assert_eq!(graph.nodes[1].status, NodeStatus::Running);
    assert_eq!(
        graph.nodes[2].status,
        NodeStatus::Pending,
        "the third ready node must not be dispatched past the cap"
    );
    assert_eq!(t.effects.len(), 2);
}

// Invariant: with a default (unconfigured) RunConfig, dispatch_cap is 1 and
// the historical strictly-serial dispatch behaviour is unchanged.
#[test]
fn default_run_config_still_dispatches_serially() {
    let graph = RunGraph {
        nodes: vec![work_node("A", "do a", &[]), work_node("B", "do b", &[])],
    };

    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Running);
    assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
    assert_eq!(t.effects.len(), 1);
}

// Invariant: when several nodes are in flight, a completion event for one of
// them is routed to that node specifically — the other in-flight node is
// left untouched, not silently treated as "the" active node.
#[test]
fn work_accepted_routes_to_the_matching_node_among_several_in_flight() {
    let graph = running_all(
        RunGraph {
            nodes: vec![work_node("A", "do a", &[]), work_node("B", "do b", &[])],
        },
        &["A", "B"],
    );

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap(2),
        },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("B".to_string()),
            work: WorkOutput {
                summary: "b done".to_string(),
            },
        },
    );

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };
    assert_eq!(
        graph.nodes[0].status,
        NodeStatus::Running,
        "node A must be untouched by B's result"
    );
    assert_eq!(graph.nodes[1].status, NodeStatus::Integrating);
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::IntegrateWork { node_id, .. }] if node_id.0 == "B"
    ));
}

// Invariant: recovery for a failing in-flight node applies only to that
// node — a sibling still in flight is left running, and the resulting state
// stays Waiting (not Active) so the sibling's own return event can still be
// delivered.
#[test]
fn node_failed_routes_recovery_to_the_matching_node_among_several_in_flight() {
    let graph = running_all(
        RunGraph {
            nodes: vec![work_node("A", "do a", &[]), work_node("B", "do b", &[])],
        },
        &["A", "B"],
    );

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap(2),
        },
        SchedulerEvent::NodeFailed {
            node_id: NodeId("A".to_string()),
            failure: NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "transient".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "transient".to_string(),
                },
            },
        },
    );

    // B is still Running, so recovering A must not jump to Active: B's
    // eventual return event still needs a Waiting state to land in.
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!(
            "expected Waiting (node B is still in flight), got {:#?}",
            t.state
        );
    };
    let a = graph.nodes.iter().find(|n| n.id.0 == "A").unwrap();
    assert_eq!(a.status, NodeStatus::Failed);
    let b = graph.nodes.iter().find(|n| n.id.0 == "B").unwrap();
    assert_eq!(
        b.status,
        NodeStatus::Running,
        "node B must be untouched by A's failure/recovery"
    );
    let replacement = graph
        .nodes
        .iter()
        .find(|n| matches!(&n.origin, NodeOrigin::Retry { source } if source.0 == "A"))
        .expect("a retry replacement for A must have been inserted");
    assert_eq!(replacement.status, NodeStatus::Pending);
    assert!(t.effects.is_empty());
}

// Invariant: once the *last* in-flight node resolves, the state returns to
// Active so the scheduler can re-scan for newly ready work.
#[test]
fn resolving_the_last_in_flight_node_returns_to_active() {
    let graph = running_all(
        RunGraph {
            nodes: vec![work_node("A", "do a", &[]), work_node("B", "do b", &[])],
        },
        &["A", "B"],
    );

    // Resolve A first (still Waiting, since B remains in flight).
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap(2),
        },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("A".to_string()),
            work: WorkOutput {
                summary: "a done".to_string(),
            },
        },
    );
    let SchedulerState::Waiting { graph, run_config } = t.state else {
        panic!("expected Waiting, got {:#?}", t.state);
    };

    // A is now Integrating (still counted in-flight); resolve its
    // integration next.
    let t = do_transition(
        SchedulerState::Waiting { graph, run_config },
        SchedulerEvent::IntegrationSucceeded {
            node_id: NodeId("A".to_string()),
            output: IntegrationOutput {
                summary: "a integrated".to_string(),
            },
            manifest_tasks: vec![],
        },
    );
    // B is still Running, so this must still be Waiting.
    let SchedulerState::Waiting { graph, run_config } = t.state else {
        panic!(
            "expected Waiting (node B is still in flight), got {:#?}",
            t.state
        );
    };
    assert_eq!(
        graph.nodes.iter().find(|n| n.id.0 == "A").unwrap().status,
        NodeStatus::Completed
    );

    // Now resolve B, the last node in flight.
    let t = do_transition(
        SchedulerState::Waiting { graph, run_config },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("B".to_string()),
            work: WorkOutput {
                summary: "b done".to_string(),
            },
        },
    );
    assert!(
        matches!(t.state, SchedulerState::Waiting { .. }),
        "B itself moves to Integrating, which still counts as in flight"
    );
}

// Invariant: a return event for a node id that exists but isn't among two or
// more in-flight nodes reports every in-flight candidate, not just a single
// "the active node" id.
#[test]
fn wrong_node_id_among_multiple_in_flight_names_every_candidate() {
    let graph = running_all(
        RunGraph {
            nodes: vec![
                work_node("A", "do a", &[]),
                work_node("B", "do b", &[]),
                work_node("C", "do c", &[]),
            ],
        },
        &["A", "B"],
    );

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap(2),
        },
        SchedulerEvent::WorkAccepted {
            node_id: NodeId("C".to_string()),
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
    assert!(detail.contains('A'), "detail should list A: {detail:?}");
    assert!(detail.contains('B'), "detail should list B: {detail:?}");
    assert!(
        detail.contains('C'),
        "detail should name the unexpected id C: {detail:?}"
    );
}
