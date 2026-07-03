use super::*;

#[test]
fn initial_state_creates_root_plan_node() {
    let request = RunRequest {
        objective: "plan the project".to_string(),
    };
    let state = SchedulerMachine::initial_state(request, RunConfig::default());
    let SchedulerState::Active { graph, .. } = state else {
        panic!("expected Active");
    };
    assert_eq!(graph.nodes.len(), 1);
    let root = &graph.nodes[0];
    assert_eq!(root.id, NodeId("root".to_string()));
    assert_eq!(root.kind, NodeKind::Plan);
    assert_eq!(root.status, NodeStatus::Pending);
    assert_eq!(root.objective, "plan the project");
    assert!(root.dependencies.is_empty());
    assert_eq!(root.attempt, 0);
    assert_eq!(root.model_tier, ModelTier::Cheap);
}

#[test]
fn plan_node_creates_work_child() {
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("P".to_string()),
            plan: PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("child-1".to_string()),
                    kind: NodeKind::Work,
                    objective: "child work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![NodeId("P".to_string())],
                    validation_plan: None,
                }],
            },
        },
    );

    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active")
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.nodes[1].kind, NodeKind::Work);
    assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
    assert_eq!(graph.nodes[1].dependencies, vec![NodeId("P".to_string())]);
}

#[test]
fn plan_with_unknown_dependency_fails_scheduler() {
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    let graph_before = running(graph, "P");
    let t = do_transition(
        SchedulerState::Waiting {
            graph: graph_before.clone(),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("P".to_string()),
            plan: PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("child-1".to_string()),
                    kind: NodeKind::Work,
                    objective: "child work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![NodeId("missing".to_string())],
                    validation_plan: None,
                }],
            },
        },
    );

    let SchedulerState::Failed { graph, reason } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 1, "no children should be inserted");
    assert_eq!(graph.nodes[0].id, NodeId("P".to_string()));
    assert_eq!(
        graph.nodes[0].status,
        NodeStatus::Running,
        "plan node should be unchanged"
    );
    let FailureReason::GraphInvariantViolation(detail) = reason else {
        panic!("expected GraphInvariantViolation, got {reason:?}");
    };
    assert!(
        detail.contains("missing"),
        "detail should mention the missing id, got: {detail:?}"
    );
    assert!(
        !detail.contains("same-batch sibling dependency"),
        "unknown dep should not be reported as sibling, got: {detail:?}"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn plan_with_valid_dependencies_still_succeeds() {
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("P".to_string()),
            plan: PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("child-1".to_string()),
                    kind: NodeKind::Work,
                    objective: "child work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![NodeId("P".to_string())],
                    validation_plan: None,
                }],
            },
        },
    );

    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 2, "child should be inserted");
    assert_eq!(graph.nodes[0].status, NodeStatus::Completed);
    assert_eq!(graph.nodes[1].status, NodeStatus::Pending);
}

#[test]
fn sibling_dependencies_are_resolved_to_graph_ids() {
    // A plan output where B depends on A from the same batch.
    // Sibling deps are now supported: B's local dep on "A" must be
    // rewritten to the actual graph NodeId assigned to A on insertion.
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("P".to_string()),
            plan: PlanOutput {
                children: vec![
                    NodeRequest {
                        id: NodeId("A".to_string()),
                        kind: NodeKind::Work,
                        objective: "step A".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    },
                    NodeRequest {
                        id: NodeId("B".to_string()),
                        kind: NodeKind::Work,
                        objective: "step B".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![NodeId("A".to_string())],
                        validation_plan: None,
                    },
                ],
            },
        },
    );

    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 3, "P + two children must be inserted");
    assert_eq!(
        graph.nodes[0].status,
        NodeStatus::Completed,
        "P must be Completed"
    );

    let child_a = graph
        .nodes
        .iter()
        .find(|n| n.objective == "step A")
        .expect("child A not found");
    let child_b = graph
        .nodes
        .iter()
        .find(|n| n.objective == "step B")
        .expect("child B not found");

    assert_eq!(child_b.dependencies.len(), 1);
    assert_eq!(
        child_b.dependencies[0], child_a.id,
        "B must depend on A's graph id"
    );
    assert_ne!(
        child_b.dependencies[0],
        NodeId("A".to_string()),
        "B must not retain the planner-local id"
    );
}

#[test]
fn planner_can_create_two_work_nodes_with_dependency() {
    // End-to-end: a plan node expands into two work nodes where B depends
    // on A. Verify the scheduler runs A first, then B after A completes.
    let graph = RunGraph {
        nodes: vec![plan_node("root", "plan a two-step task", &[])],
        next_id: 0,
    };

    // Dispatch the root plan node.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching root");
    };

    // Root plan returns two tasks: write-tests (no deps) and implement (depends on write-tests).
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("root".to_string()),
            plan: PlanOutput {
                children: vec![
                    NodeRequest {
                        id: NodeId("write-tests".to_string()),
                        kind: NodeKind::Work,
                        objective: "write tests".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    },
                    NodeRequest {
                        id: NodeId("implement".to_string()),
                        kind: NodeKind::Work,
                        objective: "implement feature".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![NodeId("write-tests".to_string())],
                        validation_plan: None,
                    },
                ],
            },
        },
    );

    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after plan expansion");
    };
    assert_eq!(graph.nodes.len(), 3, "root + two children");

    // Only write-tests must be ready; implement is blocked on it.
    let ready = SchedulerMachine::find_ready(&graph);
    assert_eq!(ready.len(), 1, "only one node must be ready");
    let write_tests_id = ready[0].clone();
    let write_tests_node = graph.nodes.iter().find(|n| n.id == write_tests_id).unwrap();
    assert_eq!(write_tests_node.objective, "write tests");

    let implement_node = graph
        .nodes
        .iter()
        .find(|n| n.objective == "implement feature")
        .unwrap();
    assert!(
        implement_node.dependencies.contains(&write_tests_id),
        "implement must depend on write-tests graph id"
    );

    // Dispatch write-tests.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching write-tests");
    };

    // write-tests completes → Integrating.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::WorkAccepted {
            node_id: write_tests_id.clone(),
            work: WorkOutput {
                summary: "tests written".to_string(),
            },
        },
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting (Integrating) after write-tests WorkAccepted");
    };

    // Integration succeeds → Active.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationSucceeded {
            node_id: write_tests_id.clone(),
            output: IntegrationOutput {
                summary: "tests integrated".to_string(),
            },
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after write-tests integration");
    };

    // Now implement must be the only ready node.
    let ready = SchedulerMachine::find_ready(&graph);
    assert_eq!(ready.len(), 1, "implement must be the only ready node");
    let implement_id = ready[0].clone();
    let implement_node = graph.nodes.iter().find(|n| n.id == implement_id).unwrap();
    assert_eq!(implement_node.objective, "implement feature");

    // Dispatch implement.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting after dispatching implement");
    };

    // implement completes → Integrating.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::WorkAccepted {
            node_id: implement_id.clone(),
            work: WorkOutput {
                summary: "feature implemented".to_string(),
            },
        },
    );
    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting (Integrating) after implement WorkAccepted");
    };

    // Integration succeeds → Active.
    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::IntegrationSucceeded {
            node_id: implement_id.clone(),
            output: IntegrationOutput {
                summary: "implementation integrated".to_string(),
            },
        },
    );
    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active after implement integration");
    };

    // All nodes terminal → Complete.
    let t = do_transition(
        SchedulerState::Active {
            graph,
            run_config: RunConfig::default(),
        },
        SchedulerEvent::Start,
    );
    let SchedulerState::Complete { graph, .. } = t.state else {
        panic!("expected Complete, got {:#?}", t.state);
    };

    let completed: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Completed)
        .collect();
    assert_eq!(completed.len(), 3, "all three nodes must be Completed");
}

#[test]
fn source_work_dispatch_includes_planned_dependent_test_targets() {
    let graph = RunGraph {
        nodes: vec![
            Node {
                target_files: vec!["main.py".to_string()],
                required_validation_targets: vec!["test_main.py".to_string()],
                ..work_node("source", "implement fibonacci", &[])
            },
            Node {
                target_files: vec!["test_main.py".to_string()],
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

    let SchedulerState::Waiting { graph, .. } = t.state else {
        panic!("expected Waiting")
    };
    assert_eq!(active_node_id(&graph), Some(NodeId("source".to_string())));
    let [
        SchedulerEffect::RunNode {
            test_plan_context, ..
        },
    ] = t.effects.as_slice()
    else {
        panic!("expected RunNode effect, got {:?}", t.effects);
    };
    assert_eq!(
        test_plan_context.required_validation_targets,
        vec!["test_main.py".to_string()]
    );
    assert_eq!(
        test_plan_context.planned_test_targets,
        vec!["test_main.py".to_string()]
    );
}

#[test]
fn source_work_dispatch_without_planned_test_target_reports_gap() {
    let graph = RunGraph {
        nodes: vec![Node {
            target_files: vec!["main.py".to_string()],
            required_validation_targets: vec!["test_main.py".to_string()],
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

    let [
        SchedulerEffect::RunNode {
            test_plan_context, ..
        },
    ] = t.effects.as_slice()
    else {
        panic!("expected RunNode effect, got {:?}", t.effects);
    };
    assert_eq!(
        test_plan_context.required_validation_targets,
        vec!["test_main.py".to_string()]
    );
    assert!(
        test_plan_context.planned_test_targets.is_empty(),
        "no objective text should create planned test metadata"
    );
}
