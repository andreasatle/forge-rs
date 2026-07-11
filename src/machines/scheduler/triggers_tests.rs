use super::*;
use crate::config::Trigger;
use crate::language::NameTargetRule;
use crate::machines::scheduler::graph::{ModelTier, Node, NodeStatus};

fn team(name: &str, trigger: Trigger) -> TeamConfig {
    TeamConfig {
        name: name.to_string(),
        northstar: String::new(),
        adapter: String::new(),
        trigger,
        name_target_rules: vec![],
    }
}

fn team_with_adapter(name: &str, trigger: Trigger, adapter: &str, northstar: &str) -> TeamConfig {
    TeamConfig {
        name: name.to_string(),
        northstar: northstar.to_string(),
        adapter: adapter.to_string(),
        trigger,
        name_target_rules: vec![],
    }
}

fn team_with_name_target_rules(
    name: &str,
    trigger: Trigger,
    name_target_rules: Vec<NameTargetRule>,
) -> TeamConfig {
    TeamConfig {
        name: name.to_string(),
        northstar: String::new(),
        adapter: String::new(),
        trigger,
        name_target_rules,
    }
}

fn record(id: &str, objective: &str, team: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        objective: objective.to_string(),
        targets: vec![],
        commit: String::new(),
        completed_at: String::new(),
        team: Some(team.to_string()),
        name: None,
    }
}

fn named_record(id: &str, objective: &str, team: &str, name: &str) -> TaskRecord {
    TaskRecord {
        name: Some(name.to_string()),
        ..record(id, objective, team)
    }
}

fn root_node() -> Node {
    Node {
        id: NodeId("root".to_string()),
        kind: NodeKind::Plan,
        team: String::new(),
        task_id: None,
        adapter: String::new(),
        northstar: String::new(),
        worker_role: None,
        objective: "build a fibonacci program".to_string(),
        target_files: vec![],
        required_validation_targets: vec![],
        dependencies: vec![],
        status: NodeStatus::Completed,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
        retry_feedback: None,
    }
}

fn run_config(teams: Vec<TeamConfig>) -> RunConfig {
    RunConfig {
        has_strong_tier: true,
        teams,
    }
}

/// A `start`-triggered team with no manifest rows yet gets its initial Plan
/// node spawned, seeded with the run's root objective.
#[test]
fn run_once_spawns_initial_plan_node() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team("planner", Trigger::Start)]);
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &[])
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "planner").collect();
    assert_eq!(spawned.len(), 1, "exactly one node spawned for the team");
    assert_eq!(spawned[0].kind, NodeKind::Plan);
    assert_eq!(spawned[0].objective, "build a fibonacci program");
    assert_eq!(spawned[0].task_id, None);
}

/// A `RunOnce`-spawned Plan node carries its own team's adapter/northstar
/// paths rather than empty strings, so the node can later be dispatched
/// under team X's own project adapter and northstar instead of whatever
/// the single-team path used.
#[test]
fn run_once_spawns_node_with_team_adapter_and_northstar() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_adapter(
        "planner",
        Trigger::Start,
        "adapters/planner.yaml",
        "northstars/planner.md",
    )]);
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &[])
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "planner").collect();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].adapter, "adapters/planner.yaml");
    assert_eq!(spawned[0].northstar, "northstars/planner.md");
}

/// Re-evaluating the same `start` trigger while the team's node is still
/// Pending must not spawn a second node: the manifest has no row yet, so
/// only the graph-based dedup check prevents a duplicate.
#[test]
fn run_once_does_not_duplicate_while_node_in_flight() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team("planner", Trigger::Start)]);
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &[])
        .expect("team triggers must apply cleanly");
    assert_eq!(
        graph.nodes.iter().filter(|n| n.team == "planner").count(),
        1
    );

    // Re-evaluate again with the same (empty) manifest, simulating another
    // unrelated node completing before the planner's node has finished.
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &[])
        .expect("team triggers must apply cleanly");
    assert_eq!(
        graph.nodes.iter().filter(|n| n.team == "planner").count(),
        1,
        "must not spawn a duplicate while the first node is still in flight"
    );
}

/// Once a team has a manifest row, `RunOnce` reports `should_run: false`, so
/// no further node is spawned even after the original node is long gone.
#[test]
fn run_once_does_not_spawn_after_manifest_row_recorded() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team("planner", Trigger::Start)]);
    let manifest = [record("t1", "do a thing", "planner")];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    assert_eq!(
        graph.nodes.iter().filter(|n| n.team == "planner").count(),
        0
    );
}

/// `after_each(planner)` fires for a task id the planner has recorded once
/// `implement` has no row of its own for that id yet, spawning a Work node
/// with the completed task's original objective text.
#[test]
fn for_tasks_spawns_work_node_with_original_objective() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
    )]);
    let manifest = [record("t1", "implement fibonacci(n: int)", "planner")];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .collect();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].kind, NodeKind::Work);
    assert_eq!(spawned[0].task_id, Some("t1".to_string()));
    assert_eq!(spawned[0].objective, "implement fibonacci(n: int)");
}

/// A `ForTasks`-spawned Work node derives its `target_files` from the
/// matched manifest task's `name` using the spawning team's
/// `name_target_rules` — the fix for the bug where these nodes were spawned
/// with `target_files: vec![]` unconditionally and so could touch no file.
#[test]
fn for_tasks_spawns_node_with_target_files_derived_from_task_name() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_name_target_rules(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
        vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "src/{name}.rs".to_string(),
        }],
    )]);
    let manifest = [named_record(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
    )];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .collect();
    assert_eq!(spawned.len(), 1);
    assert_eq!(
        spawned[0].target_files,
        vec!["src/fibonacci.rs".to_string()]
    );
}

/// When no configured `name_target_rules` matches the task's name (or the
/// team has none), applying triggers fails loudly instead of spawning a node
/// that could touch no file — never a guessed empty fallback.
#[test]
fn for_tasks_fails_when_no_rule_matches_task_name() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
    )]);
    let manifest = [named_record(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
    )];
    let err = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect_err("no name_target_rule matches, so target derivation must fail");
    assert!(
        err.contains("no name_target_rule matched"),
        "error must name the cause: {err}"
    );
}

/// A `ForTasks`-matched task with no recorded `name` at all (e.g. one
/// recorded from a completed `Work` node rather than a planner `Task`) has
/// nothing to derive a target from, so it still spawns with no target files
/// — this is a distinct, legitimate case from a name that fails to match any
/// rule.
#[test]
fn for_tasks_spawns_node_with_no_target_files_when_task_has_no_recorded_name() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
    )]);
    let manifest = [record("t1", "implement fibonacci(n: int)", "planner")];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .collect();
    assert_eq!(spawned.len(), 1);
    assert!(spawned[0].target_files.is_empty());
}

/// A `ForTasks`-spawned Work node carries its own team's adapter/northstar
/// paths, distinct from another team's, so nodes for different teams never
/// end up dispatched under a shared or borrowed adapter.
#[test]
fn for_tasks_spawns_node_with_team_adapter_and_northstar() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_adapter(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
        "adapters/implement.yaml",
        "northstars/implement.md",
    )]);
    let manifest = [record("t1", "implement fibonacci(n: int)", "planner")];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .collect();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].adapter, "adapters/implement.yaml");
    assert_eq!(spawned[0].northstar, "northstars/implement.md");
}

/// Re-evaluating `after_each` while the spawned Work node is still Pending
/// (no manifest row from `implement` yet) must not spawn a duplicate for the
/// same task id.
#[test]
fn for_tasks_does_not_duplicate_while_node_in_flight() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
    )]);
    let manifest = [record("t1", "implement fibonacci(n: int)", "planner")];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");
    assert_eq!(
        graph.nodes.iter().filter(|n| n.team == "implement").count(),
        1
    );

    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");
    assert_eq!(
        graph.nodes.iter().filter(|n| n.team == "implement").count(),
        1,
        "must not spawn a duplicate while the first node is still in flight"
    );
}

/// Once `implement` has its own manifest row for a task id, `ForTasks`
/// excludes that id, so no further node is spawned for it even after the
/// original node is marked Failed (recovery would have created a replacement
/// carrying the team/task_id forward instead).
#[test]
fn for_tasks_excludes_ids_already_recorded_by_the_team() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
    )]);
    let manifest = [
        record("t1", "implement fibonacci(n: int)", "planner"),
        record("t1", "implemented fibonacci", "implement"),
    ];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    assert_eq!(
        graph.nodes.iter().filter(|n| n.team == "implement").count(),
        0
    );
}

/// A Failed node for a (team, task) pair does not block re-spawning: the
/// dedup check only treats non-terminal-failed nodes as "already requested".
#[test]
fn for_tasks_respawns_after_prior_attempt_failed() {
    let mut graph = RunGraph {
        nodes: vec![root_node()],
    };
    graph.nodes.push(Node {
        id: NodeId("failed-attempt".to_string()),
        kind: NodeKind::Work,
        team: "implement".to_string(),
        task_id: Some("t1".to_string()),
        adapter: String::new(),
        northstar: String::new(),
        worker_role: None,
        objective: "implement fibonacci(n: int)".to_string(),
        target_files: vec![],
        required_validation_targets: vec![],
        dependencies: vec![],
        status: NodeStatus::Failed,
        attempt: 3,
        plan_depth: 0,
        model_tier: ModelTier::Strong,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
        retry_feedback: None,
    });

    let config = run_config(vec![team(
        "implement",
        Trigger::AfterEach(vec!["planner".to_string()]),
    )]);
    let manifest = [record("t1", "implement fibonacci(n: int)", "planner")];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let pending: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement" && n.status == NodeStatus::Pending)
        .collect();
    assert_eq!(pending.len(), 1, "a new attempt must be spawned");
}
