use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::config::Trigger;
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
        language_plugins: BTreeMap::new(),
        language: String::new(),
        derives_target: false,
        worker_role: None,
    }
}

/// A language plugin mapping `{stem}.rs` source files to a `{stem}_test.rs`
/// validation target, matching `plugins/rust.yaml`'s own rule — for tests
/// exercising a `derives_target: true` team's target derivation.
fn rust_language_plugins() -> BTreeMap<String, LanguageSpec> {
    let mut plugins = BTreeMap::new();
    plugins.insert(
        "rs".to_string(),
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
            functions: BTreeMap::new(),
            api_summary: None,
        },
    );
    plugins
}

/// A language plugin mapping `{stem}.py` source files to a
/// `tests/test_{stem}.py` validation target, matching `plugins/python.yaml`'s
/// own rule.
fn python_language_plugins() -> BTreeMap<String, LanguageSpec> {
    let mut plugins = BTreeMap::new();
    plugins.insert(
        "py".to_string(),
        LanguageSpec {
            extensions: vec!["py".to_string()],
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
                    pattern: "{stem}.py".to_string(),
                    target: "tests/test_{stem}.py".to_string(),
                }],
            },
            functions: BTreeMap::new(),
            api_summary: None,
        },
    );
    plugins
}

/// A team whose `ForTasks`-spawned node's target is the task's source
/// `file_path` directly (e.g. an `implementer` team) — the common case for
/// tests exercising a single `ForTasks`-spawned team.
fn team_direct(
    name: &str,
    trigger: Trigger,
    language_plugins: BTreeMap<String, LanguageSpec>,
) -> TeamConfig {
    TeamConfig {
        language_plugins,
        ..team(name, trigger)
    }
}

/// A team whose `ForTasks`-spawned node's target is derived from the task's
/// source `file_path` (e.g. a `tester` team writing the corresponding test
/// file), via `language_plugins`'s canonical validation-target derivation.
fn team_deriving(
    name: &str,
    trigger: Trigger,
    language_plugins: BTreeMap<String, LanguageSpec>,
) -> TeamConfig {
    TeamConfig {
        derives_target: true,
        language_plugins,
        ..team(name, trigger)
    }
}

fn team_with_adapter(name: &str, trigger: Trigger, adapter: &str, northstar: &str) -> TeamConfig {
    TeamConfig {
        name: name.to_string(),
        northstar: northstar.to_string(),
        adapter: adapter.to_string(),
        kind: kind_for(&trigger),
        trigger,
        language_plugins: BTreeMap::new(),
        language: String::new(),
        derives_target: false,
        worker_role: None,
    }
}

fn record(id: &str, objective: &str, team: &str) -> TaskRecord {
    TaskRecord {
        id: id.to_string(),
        objective: objective.to_string(),
        commit: String::new(),
        completed_at: String::new(),
        team: Some(team.to_string()),
        task_kv: HashMap::new(),
        depends_on: vec![],
    }
}

fn named_record(id: &str, objective: &str, team: &str, name: &str) -> TaskRecord {
    let mut r = record(id, objective, team);
    r.task_kv.insert("name".to_string(), name.to_string());
    r
}

/// A planner task row carrying the single planner-decided source `file_path`
/// — the common case for tests exercising a single `ForTasks`-spawned team.
fn record_with_file_path(
    id: &str,
    objective: &str,
    team: &str,
    name: &str,
    file_path: &str,
) -> TaskRecord {
    let mut r = named_record(id, objective, team, name);
    r.task_kv
        .insert("file_path".to_string(), file_path.to_string());
    r
}

/// A planner task row carrying both a source `file_path` and a `depends_on`
/// list, for tests exercising dependency gating on the `ForTasks` path.
fn record_with_file_path_and_deps(
    id: &str,
    objective: &str,
    team: &str,
    name: &str,
    file_path: &str,
    depends_on: Vec<&str>,
) -> TaskRecord {
    TaskRecord {
        depends_on: depends_on.into_iter().map(String::from).collect(),
        ..record_with_file_path(id, objective, team, name, file_path)
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
        team_direct(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            BTreeMap::new(),
        ),
    ]);
    let manifest = [record_with_file_path(
        "root-t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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
    let config = run_config(vec![team_direct(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        BTreeMap::new(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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
        ..team_direct(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            BTreeMap::new(),
        )
    }]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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

/// A `ForTasks`-spawned Work node whose team does not derive its target
/// (`derives_target: false`) gets `target_files` set to the matched manifest
/// task's `file_path` directly — the planner already decided this path once,
/// so there is no per-team re-derivation from the task's bare name.
#[test]
fn for_tasks_spawns_node_with_target_files_from_file_path_directly() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_direct(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        BTreeMap::new(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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

/// When a `derives_target: true` team's language plugins have no plugin
/// matching the task's `file_path` extension, applying triggers fails loudly
/// instead of spawning a node that could touch no file — never a guessed
/// empty fallback.
#[test]
fn for_tasks_fails_when_deriving_team_has_no_matching_language_plugin() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_deriving(
        "create_test",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        BTreeMap::new(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
    )];
    let err = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect_err("no language plugin matches file_path's extension, so derivation must fail");
    assert!(
        err.contains("no validation-target derivation applies"),
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
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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
    let config = run_config(vec![team_direct(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        BTreeMap::new(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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

    let config = run_config(vec![team_direct(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        BTreeMap::new(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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
        vec![team_direct(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            BTreeMap::new(),
        )],
        vec!["implement"],
    );
    let manifest = [
        record_with_file_path(
            "t1",
            "implement fibonacci(n: int)",
            "planner",
            "fibonacci",
            "src/fibonacci.rs",
        ),
        record_with_file_path_and_deps(
            "t2",
            "implement factorial(n: int)",
            "planner",
            "factorial",
            "src/factorial.rs",
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
        vec![team_direct(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            BTreeMap::new(),
        )],
        vec!["implement"],
    );
    let manifest = [
        record_with_file_path(
            "t1",
            "implement fibonacci(n: int)",
            "planner",
            "fibonacci",
            "src/fibonacci.rs",
        ),
        record("t1", "implemented fibonacci", "implement"),
        record_with_file_path_and_deps(
            "t2",
            "implement factorial(n: int)",
            "planner",
            "factorial",
            "src/factorial.rs",
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
        vec![team_direct(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            BTreeMap::new(),
        )],
        vec!["implement"],
    );
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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

/// A `ForTasks`-spawned Work node's `required_validation_targets` is the
/// sibling target derived from the same manifest row's `file_path` via the
/// team's language plugins' canonical validation-target derivation — so it
/// can never disagree with what the sibling role's own `ForTasks` node
/// actually targets (see [`implement_and_create_test_derive_matching_targets_from_the_same_file_path`]
/// for the two-team version of this proof).
#[test]
fn for_tasks_spawns_node_with_required_validation_targets_derived_from_file_path() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![team_direct(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        rust_language_plugins(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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
        "must be the derived test target for the same file_path"
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
    let config = run_config(vec![team_direct(
        "implement",
        Trigger::AfterTeams(vec!["planner".to_string()]),
        rust_language_plugins(),
    )]);
    let manifest = [record_with_file_path(
        "t1",
        "implement fibonacci(n: int)",
        "planner",
        "fibonacci",
        "src/fibonacci.rs",
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

/// Path to a real, shipped adapter YAML file (not a test fixture) — used so
/// this test exercises the actual `adapters/*.yaml` files a real run loads,
/// not a synthetic stand-in.
fn real_adapter_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters")
        .join(name)
}

/// Regression test for the `worker_role: None` bug this session fixed: before
/// the fix, `spawn_run_once`/`spawn_for_tasks` hardcoded every spawned
/// `NodeRequest::worker_role` to `None`, so `validation_plan_for_role_fn`'s
/// keyed lookup (`role_validations.get(name)`, `project_setup.rs`) always
/// missed and silently fell back to the plugin's default validation bundle —
/// even though isolated unit tests of that lookup (e.g.
/// `validation_plan_for_role_uses_adapters_tester_validation_selection`)
/// passed, because they called the function with an explicit role and never
/// exercised the actual `TeamConfig`/`NodeRequest` wiring that supplies it in
/// production.
///
/// This proves the fix end-to-end for all three real single-worker-role
/// adapters: `team.worker_role` (as `resolve_team_paths` would populate it
/// from `adapter.primary_role_key()`) survives all the way through
/// `apply_team_triggers` onto the spawned `Node::worker_role`, matching each
/// adapter's own declared `key` exactly.
#[test]
fn for_tasks_spawned_node_carries_its_teams_adapter_declared_worker_role() {
    for (adapter_file, team_name, expected_role) in [
        ("create_test.yaml", "create_test", "tester"),
        ("implement.yaml", "implement", "implementer"),
        ("pass_tests.yaml", "pass_tests", "pass_tests"),
    ] {
        let adapter = crate::project::load_adapter(&real_adapter_path(adapter_file))
            .unwrap_or_else(|e| panic!("{adapter_file} must load cleanly: {e}"));
        assert_eq!(
            adapter.primary_role_key(),
            Some(expected_role.to_string()),
            "{adapter_file}'s own declared key must be '{expected_role}'"
        );

        let graph = RunGraph {
            nodes: vec![root_node()],
        };
        let config = run_config(vec![TeamConfig {
            worker_role: adapter.primary_role_key(),
            ..team_direct(
                team_name,
                Trigger::AfterTeams(vec!["planner".to_string()]),
                python_language_plugins(),
            )
        }]);
        let manifest = [record_with_file_path(
            "t1",
            "implement fibonacci(n: int)",
            "planner",
            "fibonacci",
            "main.py",
        )];
        let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
            .expect("team triggers must apply cleanly");

        let spawned: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == team_name).collect();
        assert_eq!(spawned.len(), 1, "{adapter_file}: exactly one node spawned");
        assert_eq!(
            spawned[0].worker_role,
            Some(expected_role.to_string()),
            "{adapter_file}: spawned node must carry its team's adapter-declared worker role"
        );
    }
}

/// Regression test for the sibling implement/create_test path mismatch: with
/// a single manifest row carrying planner-decided `role_targets` for both
/// the `implementer` and `tester` roles, spawning both teams' `ForTasks`
/// nodes from that same row must produce a target-file pair that agrees by
/// construction — `implement`'s own target, and what it requires to exist
/// elsewhere (the tester's target), must equal `create_test`'s own actual
/// target, and vice versa — because `create_test`'s target is a pure
/// function of the same `record.file_path` `implement` reads directly, not
/// two independently authored tables that merely happen to agree.
///
/// This is also the reconstructed failure from run `2026-07-16-17-52-04`:
/// the planner emitted a single task with `file_path: "main.py"`, and
/// `create_test` failed with "no role_targets entry matches its role
/// 'tester'" because `role_targets` didn't cover the `create_test` team.
/// With `role_targets` gone, `create_test` derives its own target from
/// `file_path` instead of depending on the planner enumerating it — the
/// failure mode no longer exists.
#[test]
fn implement_and_create_test_derive_matching_targets_from_the_same_file_path() {
    let graph = RunGraph {
        nodes: vec![root_node()],
    };
    let config = run_config(vec![
        team_direct(
            "implement",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            python_language_plugins(),
        ),
        team_deriving(
            "create_test",
            Trigger::AfterTeams(vec!["planner".to_string()]),
            python_language_plugins(),
        ),
    ]);
    let manifest = [record_with_file_path(
        "t1",
        "A Python program in main.py that implements a Fibonacci number generator.",
        "planner",
        "fibonacci",
        "main.py",
    )];
    let graph = apply_team_triggers(graph, &NodeId("root".to_string()), &config, &manifest)
        .expect("team triggers must apply cleanly");

    let implement_node = graph
        .nodes
        .iter()
        .find(|n| n.team == "implement")
        .expect("implement must have spawned a node");
    let create_test_node = graph
        .nodes
        .iter()
        .find(|n| n.team == "create_test")
        .expect("create_test must have spawned a node");

    assert_eq!(implement_node.target_files, vec!["main.py".to_string()]);
    assert_eq!(
        create_test_node.target_files,
        vec!["tests/test_main.py".to_string()]
    );

    // implement's referee must expect exactly the path create_test actually
    // writes to, and vice versa — not a nested or otherwise-derived path
    // that happens to coincide only for some plugin shapes.
    assert_eq!(
        implement_node.required_validation_targets, create_test_node.target_files,
        "implement's required validation target must match create_test's actual target"
    );
    assert_eq!(
        create_test_node.required_validation_targets, implement_node.target_files,
        "create_test's required validation target must match implement's actual target"
    );
}
