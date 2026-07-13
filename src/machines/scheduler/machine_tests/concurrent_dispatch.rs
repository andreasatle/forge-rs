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
// node — a sibling still in flight is left running, untouched by A's
// failure/recovery. Since A's slot is now free (1 of 2 in flight), the
// state returns to Active so the freed slot can be backfilled (e.g. with
// the freshly-inserted retry replacement) without waiting for B too.
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

    // B is still Running, but A's slot is now free, so the machine returns
    // to Active to allow backfill — the driver's next Start tick will
    // re-enter Waiting (with B still in flight) before B's own return event
    // can arrive.
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!(
            "expected Active (A's slot freed even though B is still in flight), got {:#?}",
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

// Invariant: as soon as any one in-flight node fully resolves and frees a
// dispatch slot, the machine returns to Active — and the very next Start
// tick backfills that freed slot with the next ready node, without waiting
// for the rest of the original batch (here, B) to drain first.
#[test]
fn resolving_one_in_flight_node_backfills_the_freed_slot_without_waiting_for_siblings() {
    // C is Pending and immediately ready, but has no free slot to dispatch
    // into: dispatch_cap is 2 and A/B already occupy both.
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

    // Resolve A first (still Waiting, since the cap is still saturated with
    // B running and A now Integrating).
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

    // A's integration succeeds, freeing its slot for the first time.
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
    // B is still Running (1 of 2 slots occupied), but that's still below
    // cap, so the machine returns to Active instead of waiting for B too.
    let SchedulerState::Active { graph, run_config } = t.state else {
        panic!(
            "expected Active (a slot freed even though B is still in flight), got {:#?}",
            t.state
        );
    };
    assert_eq!(
        graph.nodes.iter().find(|n| n.id.0 == "A").unwrap().status,
        NodeStatus::Completed
    );

    // The next Start tick backfills the freed slot with C — B never had to
    // resolve for this dispatch to happen.
    let t = do_transition(
        SchedulerState::Active { graph, run_config },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!(
            "expected Waiting after backfilling the freed slot, got {:#?}",
            t.state
        );
    };
    assert_eq!(
        graph.nodes.iter().find(|n| n.id.0 == "B").unwrap().status,
        NodeStatus::Running,
        "B must remain untouched and in flight"
    );
    assert_eq!(
        graph.nodes.iter().find(|n| n.id.0 == "C").unwrap().status,
        NodeStatus::Running,
        "C must be backfilled into the slot A freed"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::RunNode { node_id, .. }] if node_id.0 == "C"
    ));
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
