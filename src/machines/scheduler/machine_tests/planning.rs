use super::*;

use std::collections::BTreeMap;

use crate::config::{TeamConfig, Trigger};

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
    // Invariant: the root id is a freshly minted random UUID, not a
    // hardcoded "root" string.
    assert_ne!(root.id, NodeId("root".to_string()));
    assert_eq!(root.kind, NodeKind::Plan);
    assert_eq!(root.status, NodeStatus::Pending);
    assert_eq!(root.objective, "plan the project");
    assert!(root.dependencies.is_empty());
    assert_eq!(root.attempt, 0);
    assert_eq!(root.model_tier, ModelTier::Cheap);
    // Invariant: with no `Trigger::Start` team configured (the historical
    // single-team/no-teams path), the root stays team-less.
    assert_eq!(root.team, "");
    assert_eq!(root.adapter, "");
    assert_eq!(root.northstar, "");
}

/// When `run_config.teams` includes a `Trigger::Start` team, the root node
/// must be seeded with *that* team's own `team`/`adapter`/`northstar`
/// instead of blank ones. Without this, the root's real decomposition and
/// the start-triggered team's own first node are two independent
/// mechanisms racing to plan the same objective: `apply_team_triggers` sees
/// the root as belonging to no team, cannot recognize its completed work as
/// satisfying that team's `start` trigger, and spawns a second Plan node
/// from scratch — discarding the root's work and recording it in the task
/// manifest as `team: Some("")` instead of `Some("planner")`, which then
/// hides it from any `after_teams(planner)` trigger too.
#[test]
fn initial_state_seeds_root_with_start_triggered_teams_identity() {
    let request = RunRequest {
        objective: "plan the project".to_string(),
    };
    let run_config = RunConfig {
        teams: vec![TeamConfig {
            name: "planner".to_string(),
            northstar: "northstars/planner.md".to_string(),
            adapter: "adapters/planner.yaml".to_string(),
            kind: NodeKind::Plan,
            trigger: Trigger::Start,
            name_target_rules: vec![],
            language_plugins: BTreeMap::new(),
            language: String::new(),
        }],
        ..RunConfig::default()
    };
    let state = SchedulerMachine::initial_state(request, run_config);
    let SchedulerState::Active { graph, .. } = state else {
        panic!("expected Active");
    };
    assert_eq!(graph.nodes.len(), 1);
    let root = &graph.nodes[0];
    assert_eq!(root.team, "planner");
    assert_eq!(root.adapter, "adapters/planner.yaml");
    assert_eq!(root.northstar, "northstars/planner.md");
    assert_eq!(
        root.origin,
        NodeOrigin::Root,
        "identity unification does not change origin"
    );
}

#[test]
fn plan_node_creates_work_child() {
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan something", &[])],
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
                    team: String::new(),
                    task_id: None,
                    adapter: String::new(),
                    northstar: String::new(),
                    worker_role: None,
                    objective: "child work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![NodeId("P".to_string())],
                    validation_plan: None,
                }],
                tasks: vec![],
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
                    team: String::new(),
                    task_id: None,
                    adapter: String::new(),
                    northstar: String::new(),
                    worker_role: None,
                    objective: "child work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![NodeId("missing".to_string())],
                    validation_plan: None,
                }],
                tasks: vec![],
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
                    team: String::new(),
                    task_id: None,
                    adapter: String::new(),
                    northstar: String::new(),
                    worker_role: None,
                    objective: "child work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![NodeId("P".to_string())],
                    validation_plan: None,
                }],
                tasks: vec![],
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
                        team: String::new(),
                        task_id: None,
                        adapter: String::new(),
                        northstar: String::new(),
                        worker_role: None,
                        objective: "step A".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    },
                    NodeRequest {
                        id: NodeId("B".to_string()),
                        kind: NodeKind::Work,
                        team: String::new(),
                        task_id: None,
                        adapter: String::new(),
                        northstar: String::new(),
                        worker_role: None,
                        objective: "step B".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![NodeId("A".to_string())],
                        validation_plan: None,
                    },
                ],
                tasks: vec![],
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
                        team: String::new(),
                        task_id: None,
                        adapter: String::new(),
                        northstar: String::new(),
                        worker_role: None,
                        objective: "write tests".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![],
                        validation_plan: None,
                    },
                    NodeRequest {
                        id: NodeId("implement".to_string()),
                        kind: NodeKind::Work,
                        team: String::new(),
                        task_id: None,
                        adapter: String::new(),
                        northstar: String::new(),
                        worker_role: None,
                        objective: "implement feature".to_string(),
                        target_files: vec![],
                        required_validation_targets: vec![],
                        dependencies: vec![NodeId("write-tests".to_string())],
                        validation_plan: None,
                    },
                ],
                tasks: vec![],
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
            manifest_tasks: vec![],
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
            manifest_tasks: vec![],
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
