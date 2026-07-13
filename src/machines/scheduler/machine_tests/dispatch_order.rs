use super::*;

fn node_id_by_objective(graph: &RunGraph, objective: &str) -> NodeId {
    graph
        .nodes
        .iter()
        .find(|n| n.objective == objective)
        .unwrap_or_else(|| panic!("no node with objective {objective:?} in graph: {graph:#?}"))
        .id
        .clone()
}

// Invariant: dispatch is depth-first, not breadth-first. When a Plan node's
// output leaves a deep branch and a shallow sibling both ready at once, the
// scheduler must keep drilling into whatever branch it just expanded — all
// the way down to a genuine Work node — before it ever dispatches the
// shallow sibling, even though the sibling has been ready the whole time.
#[test]
fn dispatch_drills_into_the_deep_branch_before_touching_the_shallow_sibling() {
    let cap = RunConfig {
        dispatch_cap: 1,
        ..RunConfig::default()
    };

    let graph = RunGraph {
        nodes: vec![plan_node("root", "plan the tree", &[])],
    };

    // Dispatch the root Plan node.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching root");
    };

    // Root decomposes into two Plan siblings: "shallow" (listed first, will
    // never recurse further in this test) and "deep-plan" (listed second,
    // the branch that keeps expanding). Both become ready in the same tick.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("root".to_string()),
            plan: PlanOutput {
                children: vec![
                    NodeRequest {
                        id: NodeId("shallow".to_string()),
                        kind: NodeKind::Plan,
                        team: String::new(),
                        task_id: None,
                        adapter: String::new(),
                        northstar: String::new(),
                        worker_role: None,
                        objective: "shallow sibling".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    },
                    NodeRequest {
                        id: NodeId("deep-plan".to_string()),
                        kind: NodeKind::Plan,
                        team: String::new(),
                        task_id: None,
                        adapter: String::new(),
                        northstar: String::new(),
                        worker_role: None,
                        objective: "deep branch root".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    },
                ],
                tasks: vec![],
            },
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after root's plan expansion");
    };
    assert_eq!(graph.nodes.len(), 3, "root + shallow + deep-plan");
    let shallow_id = node_id_by_objective(&graph, "shallow sibling");
    let deep_plan_id = node_id_by_objective(&graph, "deep branch root");

    // Both "shallow" and "deep-plan" are ready. Depth-first dispatch must
    // pick "deep-plan" (the more recently inserted) over "shallow", even
    // though both became ready in the same tick.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching deep-plan");
    };
    assert_eq!(
        active_node_id(&graph),
        Some(deep_plan_id.clone()),
        "depth-first dispatch must descend into deep-plan before shallow"
    );
    let shallow = graph.nodes.iter().find(|n| n.id == shallow_id).unwrap();
    assert_eq!(
        shallow.status,
        NodeStatus::Pending,
        "shallow must still be untouched even though it has been ready since the previous tick"
    );

    // "deep-plan" decomposes one level further into a genuine Work node.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: deep_plan_id,
            plan: PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("deep-work".to_string()),
                    kind: NodeKind::Work,
                    team: String::new(),
                    task_id: None,
                    adapter: String::new(),
                    northstar: String::new(),
                    worker_role: None,
                    objective: "do the deep leaf work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![],
                    validation_plan: None,
                }],
                tasks: vec![],
            },
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after deep-plan's expansion");
    };
    assert_eq!(
        graph.nodes.len(),
        4,
        "root + shallow + deep-plan + deep-work"
    );
    let deep_work_id = node_id_by_objective(&graph, "do the deep leaf work");

    // "deep-work" (freshly inserted) and "shallow" (ready since the very
    // first tick) are both ready now. Depth-first dispatch must still prefer
    // "deep-work" over "shallow" — reaching the deep branch's actual
    // Work/Task node before the shallow sibling is ever expanded.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching deep-work");
    };
    assert_eq!(
        active_node_id(&graph),
        Some(deep_work_id.clone()),
        "the deep branch's Work node must be reached before the shallow sibling is dispatched"
    );
    let shallow = graph.nodes.iter().find(|n| n.id == shallow_id).unwrap();
    assert_eq!(
        shallow.status,
        NodeStatus::Pending,
        "shallow must never have been expanded while the deep branch was still unfinished"
    );

    // Resolve "deep-work" so the deep branch fully bottoms out.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::WorkAccepted {
            node_id: deep_work_id.clone(),
            work: WorkOutput {
                summary: "deep leaf done".to_string(),
            },
        },
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting (Integrating) after deep-work WorkAccepted");
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: cap.clone(),
        },
        SchedulerEvent::IntegrationSucceeded {
            node_id: deep_work_id,
            output: IntegrationOutput {
                summary: "deep leaf integrated".to_string(),
            },
            manifest_tasks: vec![],
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after deep-work integration");
    };

    // Only now, with the entire deep branch terminal, does "shallow" finally
    // become the sole ready node and get its turn.
    let ready = SchedulerMachine::find_ready(&graph);
    assert_eq!(
        ready,
        vec![shallow_id.clone()],
        "shallow must be the only ready node once the deep branch is fully drained"
    );
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: cap,
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after finally dispatching shallow");
    };
    assert_eq!(
        active_node_id(&graph),
        Some(shallow_id),
        "shallow must be dispatched once the deep branch has fully drained, proving it \
         is deferred rather than starved"
    );
}
