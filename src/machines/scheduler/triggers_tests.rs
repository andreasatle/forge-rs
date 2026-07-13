use std::collections::BTreeMap;

use super::*;
use crate::config::Trigger;
use crate::language::NameTargetRule;
use crate::language::spec::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};
use crate::machines::scheduler::graph::{ModelTier, Node, NodeKind, NodeStatus};
use crate::validation::ValidationTargetRule;

fn kind_for(trigger: &Trigger) -> NodeKind {
    match trigger {
        Trigger::Start => NodeKind::Plan,
        Trigger::AfterTeams(_) => NodeKind::Work,
    }
}

fn team(name: &str, trigger: Trigger) -> TeamConfig {
    TeamConfig {
        name: name.to_string(),
        northstar: String::new(),
        adapter: String::new(),
        kind: kind_for(&trigger),
        trigger,
        name_target_rules: vec![],
        language_plugins: BTreeMap::new(),
    }
}

fn team_with_adapter(name: &str, trigger: Trigger, adapter: &str, northstar: &str) -> TeamConfig {
    TeamConfig {
        name: name.to_string(),
        northstar: northstar.to_string(),
        adapter: adapter.to_string(),
        kind: kind_for(&trigger),
        trigger,
        name_target_rules: vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "src/{name}.rs".to_string(),
        }],
        language_plugins: BTreeMap::new(),
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
        kind: kind_for(&trigger),
        trigger,
        name_target_rules,
        language_plugins: BTreeMap::new(),
    }
}

/// A minimal language plugin that requires tests and derives a `test_{stem}.rs`
/// validation target for every `.rs` source, keyed under the `"rs"` extension —
/// mirrors what `resolve_team_paths` merges onto `TeamConfig::language_plugins`
/// from a real adapter's plugins at config-load time.
fn rs_plugin_requiring_tests() -> LanguageSpec {
    LanguageSpec {
        extensions: vec!["rs".to_string()],
        identity: String::new(),
        context: String::new(),
        instructions: String::new(),
        constraints: String::new(),
        init: LanguageInitSpec {
            gitignore: vec![],
            commands: vec![],
        },
        validation: LanguageValidationSpec {
            runs_tests: true,
            commands: vec![],
            validation_targets: vec![ValidationTargetRule {
                pattern: "{stem}.rs".to_string(),
                target: "{stem}_test.rs".to_string(),
            }],
        },
        plugin_roles: vec![],
        api_summary: None,
        name_target_rules: vec![],
    }
}

/// A team whose `name_target_rules` derives a `.rs` target file from any task
/// name, and whose `language_plugins` declares that `.rs` sources require a
/// test target — the wiring needed to exercise real (non-hardcoded-empty)
/// `required_validation_targets` computation on the `ForTasks` spawn path.
fn team_with_catchall_rule_and_validation_targets(name: &str, trigger: Trigger) -> TeamConfig {
    TeamConfig {
        language_plugins: BTreeMap::from([("rs".to_string(), rs_plugin_requiring_tests())]),
        ..team_with_catchall_rule(name, trigger)
    }
}

/// A team whose `name_target_rules` matches any task name, for tests that
/// exercise `ForTasks` spawning but don't care what target file results.
fn team_with_catchall_rule(name: &str, trigger: Trigger) -> TeamConfig {
    team_with_name_target_rules(
        name,
        trigger,
        vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "src/{name}.rs".to_string(),
        }],
    )
}

fn record(id: &str, objective: &str, team: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        objective: objective.to_string(),
        commit: String::new(),
        completed_at: String::new(),
        team: Some(team.to_string()),
        name: None,
        depends_on: vec![],
    }
}

fn named_record(id: &str, objective: &str, team: &str, name: &str) -> TaskRecord {
    TaskRecord {
        name: Some(name.to_string()),
        ..record(id, objective, team)
    }
}

fn named_record_with_deps(
    id: &str,
    objective: &str,
    team: &str,
    name: &str,
    depends_on: Vec<&str>,
) -> TaskRecord {
    TaskRecord {
        depends_on: depends_on.into_iter().map(String::from).collect(),
        ..named_record(id, objective, team, name)
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
        terminal_teams: vec![],
        dispatch_cap: 1,
    }
}

fn run_config_with_terminal_teams(teams: Vec<TeamConfig>, terminal_teams: Vec<&str>) -> RunConfig {
    RunConfig {
        has_strong_tier: true,
        teams,
        terminal_teams: terminal_teams.into_iter().map(String::from).collect(),
        dispatch_cap: 1,
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

/// `spawn_run_once` reads its spawned node's `kind` from `team.kind`, not
/// from the fact that a `RunOnce` decision fired — a `RunOnce`-triggered
/// team declaring `kind: Work` must still get a Work node, proving `kind`
/// is the actual source of truth rather than inferred from the trigger.
#[test]
fn run_once_spawns_node_with_teams_declared_kind_not_inferred_from_trigger() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![TeamConfig {
        kind: NodeKind::Work,
        ..team("planner", Trigger::Start)
    }]);
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &[])
        .expect("team triggers must apply cleanly");

    let spawned: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "planner").collect();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].kind, NodeKind::Work);
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

/// Proves the fix's central guarantee: once the root node is itself seeded
/// with the `planner` team's identity (as `SchedulerMachine::initial_state`
/// now does for a `Trigger::Start` team — see
/// `machine_tests::planning::initial_state_seeds_root_with_start_triggered_teams_identity`),
/// its own completed decomposition is recorded to the manifest correctly
/// attributed to "planner" (the completing node's own `team` field, not the
/// empty string a blank-identity root previously carried). `planner`'s
/// `start` trigger then sees `should_run: false` and does not spawn a
/// second Plan node from scratch, discarding the root's completed work.
#[test]
fn root_lineage_task_attributed_to_start_team_does_not_restart() {
    let graph = RunGraph {
        nodes: vec![Node {
            team: "planner".to_string(),
            adapter: "adapters/planner.yaml".to_string(),
            northstar: "northstars/planner.md".to_string(),
            ..root_node()
        }],
    };
    let config = run_config(vec![team_with_adapter(
        "planner",
        Trigger::Start,
        "adapters/planner.yaml",
        "northstars/planner.md",
    )]);
    // The root node's own decomposition, correctly attributed to "planner"
    // because the completing node's own `team` field is "planner" now.
    let manifest = [named_record(
        "root-t1",
        "decompose the objective",
        "planner",
        "root_t1",
    )];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let planner_nodes: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "planner").collect();
    assert_eq!(
        planner_nodes.len(),
        1,
        "the root node's own decomposition already satisfies the planner team's start \
         trigger; a second Plan node must not be spawned, discarding completed work; \
         graph: {graph:#?}"
    );
    assert_eq!(
        planner_nodes[0].id,
        NodeId("root".to_string()),
        "the sole planner-team node must be the root node itself, not a duplicate spawn; \
         graph: {graph:#?}"
    );
}

/// Parallel to the test above: once the root-lineage task is correctly
/// attributed to "planner" in the manifest, a downstream
/// `after_teams(planner)` trigger recognizes it as an ordinary planner
/// completion and fires — proving the fix doesn't just silence the
/// duplicate restart but threads the correct attribution through to teams
/// that depend on it, which previously could never fire off a root-lineage
/// task (it carried `team: Some("")`, not `Some("planner")`).
#[test]
fn implement_after_teams_planner_fires_off_root_lineage_task() {
    let graph = RunGraph {
        nodes: vec![Node {
            team: "planner".to_string(),
            adapter: "adapters/planner.yaml".to_string(),
            northstar: "northstars/planner.md".to_string(),
            ..root_node()
        }],
    };
    let config = run_config(vec![
        team_with_adapter(
            "planner",
            Trigger::Start,
            "adapters/planner.yaml",
            "northstars/planner.md",
        ),
        team_with_catchall_rule(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
        ),
    ]);
    let manifest = [named_record(
        "root-t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
    )];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let planner_nodes: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "planner").collect();
    assert_eq!(
        planner_nodes.len(),
        1,
        "planner's own start trigger must not also re-fire in this same evaluation; \
         graph: {graph:#?}"
    );
    let implement_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .collect();
    assert_eq!(
        implement_nodes.len(),
        1,
        "implement's after_teams(planner) trigger must fire off the root-lineage task; \
         graph: {graph:#?}"
    );
    assert_eq!(implement_nodes[0].kind, NodeKind::Work);
    assert_eq!(implement_nodes[0].task_id, Some("root-t1".to_string()));
    assert_eq!(implement_nodes[0].objective, "implement fibonacci(n: int)");
}

/// `after_teams(planner)` fires for a task id the planner has recorded once
/// `implement` has no row of its own for that id yet, spawning a Work node
/// with the completed task's original objective text.
#[test]
fn for_tasks_spawns_work_node_with_original_objective() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_catchall_rule(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
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
    assert_eq!(spawned[0].kind, NodeKind::Work);
    assert_eq!(spawned[0].task_id, Some("t1".to_string()));
    assert_eq!(spawned[0].objective, "implement fibonacci(n: int)");
}

/// `spawn_for_tasks` reads its spawned nodes' `kind` from `team.kind`, not
/// from the fact that a `ForTasks` decision fired — a `ForTasks`-triggered
/// team declaring `kind: Plan` must still get Plan nodes, proving `kind` is
/// the actual source of truth rather than inferred from the trigger.
#[test]
fn for_tasks_spawns_node_with_teams_declared_kind_not_inferred_from_trigger() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![TeamConfig {
        kind: NodeKind::Plan,
        ..team_with_catchall_rule(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
        )
    }]);
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
    assert_eq!(spawned[0].kind, NodeKind::Plan);
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
        Trigger::AfterTeams(vec!["planner".to_string()]),
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
        Trigger::AfterTeams(vec!["planner".to_string()]),
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
        Trigger::AfterTeams(vec!["planner".to_string()]),
        "adapters/implement.yaml",
        "northstars/implement.md",
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
    assert_eq!(spawned[0].adapter, "adapters/implement.yaml");
    assert_eq!(spawned[0].northstar, "northstars/implement.md");
}

/// Re-evaluating `after_teams` while the spawned Work node is still Pending
/// (no manifest row from `implement` yet) must not spawn a duplicate for the
/// same task id.
#[test]
fn for_tasks_does_not_duplicate_while_node_in_flight() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_catchall_rule(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
    )]);
    let manifest = [named_record(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
    )];
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
        Trigger::AfterTeams(vec!["planner".to_string()]),
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

    let config = run_config(vec![team_with_catchall_rule(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
    )]);
    let manifest = [named_record(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
    )];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let pending: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement" && n.status == NodeStatus::Pending)
        .collect();
    assert_eq!(pending.len(), 1, "a new attempt must be spawned");
}

/// A `ForTasks` candidate whose `depends_on` names a task with no completion
/// row yet from a terminal team must not spawn: `t2` depends on `t1`, and
/// `implement` (the only terminal team) has not yet recorded `t1`.
#[test]
fn for_tasks_does_not_spawn_when_dependency_unsatisfied() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config_with_terminal_teams(
        vec![team_with_catchall_rule(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
        )],
        vec!["implement"],
    );
    let manifest = [
        named_record("t1", "implement fibonacci(n: int)", "planner", "fibonacci"),
        named_record_with_deps(
            "t2",
            "implement factorial(n: int)",
            "planner",
            "factorial",
            vec!["t1"],
        ),
    ];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned_ids: Vec<Option<String>> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .map(|n| n.task_id.clone())
        .collect();
    assert_eq!(
        spawned_ids,
        vec![Some("t1".to_string())],
        "t2 must not spawn while its dependency t1 has no completion from the terminal team"
    );
}

/// Once every terminal team has recorded a completion for `t1`, `t2` (which
/// depends on it) becomes eligible and spawns.
#[test]
fn for_tasks_spawns_once_all_terminal_teams_complete_dependency() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config_with_terminal_teams(
        vec![team_with_catchall_rule(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
        )],
        vec!["implement"],
    );
    let manifest = [
        named_record("t1", "implement fibonacci(n: int)", "planner", "fibonacci"),
        record("t1", "implemented fibonacci", "implement"),
        named_record_with_deps(
            "t2",
            "implement factorial(n: int)",
            "planner",
            "factorial",
            vec!["t1"],
        ),
    ];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned_ids: Vec<Option<String>> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "implement")
        .map(|n| n.task_id.clone())
        .collect();
    assert_eq!(
        spawned_ids,
        vec![Some("t2".to_string())],
        "t2 must spawn once its dependency t1 has a completion from every terminal team"
    );
}

/// A task with no `depends_on` is unaffected by `terminal_teams` being
/// configured — today's no-dependency behavior is unchanged.
#[test]
fn for_tasks_unaffected_when_no_depends_on() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config_with_terminal_teams(
        vec![team_with_catchall_rule(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
        )],
        vec!["implement"],
    );
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
    assert_eq!(spawned[0].task_id, Some("t1".to_string()));
}

/// A `ForTasks`-spawned Work node's `required_validation_targets` is derived
/// for real from the spawning team's language plugins, mirroring
/// `required_test_targets_fn` on the planner path — not hardcoded to
/// `vec![]` regardless of what the team's adapter declares.
#[test]
fn for_tasks_spawns_node_with_required_validation_targets_from_team_language_plugins() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_catchall_rule_and_validation_targets(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
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
    assert_eq!(
        spawned[0].required_validation_targets,
        vec!["src/fibonacci_test.rs".to_string()],
        "must derive a real required validation target from the team's language plugin, not vec![]"
    );
}

/// The `required_validation_targets` computed for a `ForTasks`-spawned node is
/// not just populated but actually enforced: `validate_required_tests_completed`
/// fails while the derived test target is incomplete, and passes once it is —
/// proving the gate is non-vacuous on the multi-team spawn path, not merely
/// that a value was stamped somewhere no one reads.
#[test]
fn for_tasks_spawned_node_required_validation_target_is_enforced_by_the_gate() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_with_catchall_rule_and_validation_targets(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
    )]);
    let manifest = [named_record(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
    )];
    let mut graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let spawned_id = graph
        .nodes
        .iter()
        .find(|n| n.team == "implement")
        .expect("a node must have spawned")
        .id
        .clone();
    graph = graph.mark_node(&spawned_id, NodeStatus::Completed);

    let err = graph
        .validate_required_tests_completed()
        .expect_err("the derived required validation target has no completed node yet");
    assert!(
        err.contains("src/fibonacci_test.rs"),
        "failure must name the missing required validation target; got: {err}"
    );

    graph.nodes.push(Node {
        id: NodeId("tests".to_string()),
        kind: NodeKind::Work,
        team: "implement".to_string(),
        task_id: None,
        adapter: String::new(),
        northstar: String::new(),
        worker_role: None,
        objective: "write tests".to_string(),
        target_files: vec!["src/fibonacci_test.rs".to_string()],
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
    });
    graph
        .validate_required_tests_completed()
        .expect("the required validation target is now completed by another node");
}
