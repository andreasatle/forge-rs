use super::*;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use crate::config::ForgeConfig;
use crate::machines::scheduler::run_scheduler_with_telemetry;
use crate::node_runner::ProjectRuntimeSetup;
use crate::providers::{ProviderClient, ProviderError, ProviderRequest, ProviderResponse};

/// A trivial planner-only adapter YAML: no worker roles, since this fixture's
/// `recursive` team never produces a `Work` node — its planner recurses
/// through further `Plan` nodes and terminates in a `kind: "task"` batch
/// (pure planner intent, recorded to the manifest, per
/// [`crate::node_runner::planner::PlannerOutputKind::Task`]). `marker` is
/// baked into the planner producer's identity so a rendered prompt would
/// reveal which adapter a node actually ran under, matching the fixture
/// convention in `multi_team::adapter_yaml`.
fn adapter_yaml(marker: &str) -> String {
    format!(
        r#"
planner:
  producer:
    identity: "{marker}: planner producer"
    context: ""
    instructions: "Emit either {{\"kind\":\"plan\",...}} or {{\"kind\":\"task\",...}}."
    constraints: ""
  critic:
    identity: "{marker}: planner critic"
    context: ""
    instructions: ""
    constraints: ""
  referee:
    identity: "{marker}: planner referee"
    context: ""
    instructions: ""
    constraints: ""
workers: []
context_files: []
plugins: []
"#
    )
}

/// Records every prompt it is called with and replays scripted responses in
/// order. Identical in shape to `multi_team::RecordingScriptedProvider`;
/// `Mutex` (rather than `RefCell`) is required only so the type is `Sync`,
/// as the scheduler driver shares `&NodeRunner` across dispatch threads.
/// This fixture runs with `dispatch_cap: 1`, so at most one node is ever in
/// flight and the scripted queue is still consumed in a fixed, predictable
/// order — just not FIFO-by-insertion (see the dispatch-order comment below).
struct RecordingScriptedProvider {
    prompts: Mutex<Vec<String>>,
    responses: Mutex<VecDeque<String>>,
}

impl RecordingScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
        }
    }
}

impl ProviderClient for RecordingScriptedProvider {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.prompts
            .lock()
            .expect("mutex poisoned")
            .push(request.prompt.clone());
        let content = self
            .responses
            .lock()
            .expect("mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                panic!(
                    "RecordingScriptedProvider: responses exhausted; prompt was:\n{}",
                    request.prompt
                )
            });
        Ok(ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

/// End-to-end: a `forge.yaml` fixture with a single `recursive` team
/// (`trigger: start`), whose adapter's planner recurses through real
/// multi-task `"kind":"plan"` decompositions — each level splits into two
/// tasks, since a single-task `"kind":"plan"` output is a no-op decomposition
/// that `PlannerOutputProcessor::into_plan` now short-circuits into a
/// terminal `"kind":"task"` output rather than a further `Plan` node (see
/// `plan_kind_output_with_single_task_becomes_terminal_task` in
/// `node_runner::planner::tests`) — before branches finally emit
/// `"kind":"task"`, driven through the real `ForgeConfig::from_file` ->
/// `SchedulerMachine` -> `SchedulerHandler` -> `DeliberatingNodeRunner`
/// stack, with a scripted provider standing in for the LLM.
///
/// This targets the exact mechanism behind a real team-loss bug: a
/// team-owned `Plan` node's children must inherit that team's
/// `team`/`adapter`/`northstar` (see `PlannerOutputProcessor::into_plan` and
/// `RunGraph::insert_children`), and each recursively spawned `Plan` child
/// must in turn pass its own (still team-owned) fields to its own children
/// when the scheduler dispatches it and it plans again. `into_plan` is
/// otherwise only unit-tested by calling it directly on a hand-built
/// `PlannerOutput`, which cannot exercise a node created from a *previous*
/// `into_plan` call recursing a second time through the real dispatch loop.
///
/// Proves: every node in the `recursive` team's Plan tree — including the
/// root node itself, which `SchedulerMachine::initial_state` seeds with
/// `recursive`'s own team/adapter/northstar since `recursive` is this run's
/// sole `Trigger::Start` team — carries the team's own
/// `team`/`adapter`/`northstar`, not the top-level/default adapter's (empty)
/// fields.
#[test]
fn team_owned_plan_node_propagates_team_through_recursive_plan_children() {
    let temp = TempDirectory::new("recursive-plan-team-e2e");

    // Seed a real bare artifact repo, exactly as `load_or_create_artifact`
    // expects to find (or as `fixture()` builds for other handler tests).
    let seed_path = temp.join("seed");
    fs::create_dir(&seed_path).expect("failed to create seed directory");
    git(&seed_path, &["init", "--quiet", "--initial-branch=main"]);
    git(
        &seed_path,
        &["config", "user.name", "Recursive Plan Team Test"],
    );
    git(
        &seed_path,
        &[
            "config",
            "user.email",
            "recursive-plan-team-test@example.invalid",
        ],
    );
    fs::write(seed_path.join("seed.txt"), "seed\n").expect("failed to write seed file");
    git(&seed_path, &["add", "seed.txt"]);
    git(&seed_path, &["commit", "--quiet", "-m", "Initial"]);
    let repo_path = temp.join("artifact.git");
    git_clone_bare(&seed_path, &repo_path);
    let commit_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let artifact = Artifact {
        repo_path: repo_path.clone(),
        branch: "main".to_owned(),
        commit_sha,
    };

    // Fixture adapters and northstars: a distinct top-level/default adapter
    // (required by `ForgeConfig::from_file` but never actually dispatched to
    // in this run, since `recursive` is the sole `Trigger::Start` team and
    // the root node is seeded with its fields) plus the `recursive` team's
    // own, so a team-loss bug (children silently falling back to the
    // top-level adapter's empty team/adapter/northstar) would be visible.
    let root_adapter_path = temp.join("root_adapter.yaml");
    fs::write(&root_adapter_path, adapter_yaml("ROOT")).expect("write root adapter");
    let recursive_adapter_path = temp.join("recursive_adapter.yaml");
    fs::write(&recursive_adapter_path, adapter_yaml("RECURSIVE-TEAM"))
        .expect("write recursive adapter");

    let recursive_northstar_path = temp.join("recursive_northstar.txt");
    fs::write(
        &recursive_northstar_path,
        "RECURSIVE-TEAM-NORTHSTAR: decompose repeatedly before producing tasks.\n",
    )
    .expect("write recursive northstar");

    let forge_yaml = format!(
        r#"
objective: "Ship a recursive-plan fixture."
artifact:
  repo_path: "{repo_path}"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://127.0.0.1:1"
      model: "unused-in-this-test"
      n_predict: 64
telemetry:
  directory: "{telemetry_dir}"
adapter: "{root_adapter}"
teams:
  - name: recursive
    northstar: "{recursive_northstar}"
    adapter: "{recursive_adapter}"
    kind: plan
    trigger: start
"#,
        repo_path = repo_path.display(),
        telemetry_dir = temp.join("telemetry").display(),
        root_adapter = root_adapter_path.display(),
        recursive_northstar = recursive_northstar_path.display(),
        recursive_adapter = recursive_adapter_path.display(),
    );
    let forge_yaml_path = temp.join("forge.yaml");
    fs::write(&forge_yaml_path, forge_yaml).expect("write forge.yaml");

    // Load exactly the way `forge start` does: `ForgeConfig::from_file`
    // resolves and validates the team's adapter/northstar up front.
    let config = ForgeConfig::from_file(&forge_yaml_path.to_string_lossy())
        .expect("forge.yaml fixture must load and validate");
    assert_eq!(config.teams.len(), 1);

    // Wire the top-level/default adapter the same way `RunSession::drive`
    // does. It is required by `ForgeConfig::from_file` and supplies the
    // `DeliberatingNodeRunner`'s fallback wiring, but nothing in this run
    // actually dispatches under it: the root node is seeded with
    // `recursive`'s own team/adapter/northstar since `recursive` is the
    // run's sole `Trigger::Start` team.
    let root_setup = ProjectRuntimeSetup::build(Path::new(&config.adapter), None)
        .expect("root adapter must load");

    // Response order, matching how the scheduler will actually dispatch.
    // `dispatch_cap: 1` means exactly one node is ever in flight, and
    // `RunGraph::find_ready` scans for the *most recently inserted* ready
    // node first (see `RunGraph::insert_children`), so within a batch of
    // siblings the last-listed child dispatches first, and the scheduler
    // drills all the way into whatever branch it just expanded before ever
    // returning to an earlier-inserted sibling. Every Plan-kind node goes
    // through one producer/critic/referee cycle:
    //   1. The root Plan node *is* the `recursive` team's own Plan node: it
    //      emits `"kind":"plan"` with two tasks ("plan-a", "plan-b") — a
    //      real decomposition, so both become Plan children.
    //   2. "plan-b" is dispatched next: it was listed second, so it's the
    //      more-recently-inserted of the two and wins over "plan-a". It
    //      emits `"kind":"task"`, terminating immediately.
    //   3. "plan-a" is dispatched next — the only node left ready once
    //      "plan-b" completes. It itself emits `"kind":"plan"` with two
    //      tasks ("leaf-a1", "leaf-a2"), proving a Plan node created by a
    //      *previous* recursive `into_plan` call still propagates the
    //      team's fields when it plans again.
    //   4. "leaf-a2" is dispatched next (listed second, so more recently
    //      inserted than "leaf-a1"), then "leaf-a1" last — both emitting
    //      `"kind":"task"` to terminate.
    let provider = RecordingScriptedProvider::from_strs(&[
        // 1. root/recursive-team Plan node
        r#"{"kind":"plan","tasks":[{"id":"plan-a","objective":"decompose branch a","name":"branch_a","depends_on":[]},{"id":"plan-b","objective":"decompose branch b","name":"branch_b","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"root critic ok"}"#,
        r#"{"status":"accepted","content":"root referee approved"}"#,
        // 2. "plan-b" Plan child: terminates immediately
        r#"{"kind":"task","tasks":[{"id":"leaf-b","objective":"do the leaf b work","name":"leaf_b","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"plan-b critic ok"}"#,
        r#"{"status":"accepted","content":"plan-b referee approved"}"#,
        // 3. "plan-a" Plan child: decomposes further
        r#"{"kind":"plan","tasks":[{"id":"leaf-a1","objective":"decompose leaf a1","name":"leaf_a1_plan","depends_on":[]},{"id":"leaf-a2","objective":"decompose leaf a2","name":"leaf_a2_plan","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"plan-a critic ok"}"#,
        r#"{"status":"accepted","content":"plan-a referee approved"}"#,
        // 4. "leaf-a2" Plan grandchild: terminates immediately
        r#"{"kind":"task","tasks":[{"id":"leaf-a2-task","objective":"do the leaf a2 work","name":"leaf_a2","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"leaf-a2 critic ok"}"#,
        r#"{"status":"accepted","content":"leaf-a2 referee approved"}"#,
        // 5. "leaf-a1" Plan grandchild: terminates immediately
        r#"{"kind":"task","tasks":[{"id":"leaf-a1-task","objective":"do the leaf a1 work","name":"leaf_a1","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"leaf-a1 critic ok"}"#,
        r#"{"status":"accepted","content":"leaf-a1 referee approved"}"#,
    ]);

    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_role_policy(root_setup.role_policy)
        .with_required_test_targets_fn(root_setup.required_test_targets_fn)
        .with_context_file_names(root_setup.context_file_names)
        .with_api_summary_command(root_setup.api_summary_command)
        .with_language_plugins(root_setup.language_plugins)
        .with_validation_plan_for_role_fn(root_setup.validation_plan_for_role_fn);

    let handler = SchedulerHandler::with_artifact(runner, artifact);
    let telemetry = VecTelemetry::new();

    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: config.root_objective().to_string(),
        },
        RunConfig {
            has_strong_tier: false,
            teams: config.teams.clone(),
            terminal_teams: config.terminal_teams.clone(),
            dispatch_cap: 1,
        },
    );

    let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, &telemetry);

    let SchedulerTerminalOutput::Complete { graph, .. } = output else {
        panic!("expected the run to complete, got: {output:#?}");
    };

    // The whole Plan tree was actually spawned under the "recursive" team:
    // the root node itself (the team's start-triggered node), its two
    // children ("plan-a", "plan-b"), and "plan-a"'s two further children
    // ("leaf-a1", "leaf-a2").
    let recursive_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "recursive")
        .collect();
    assert_eq!(
        recursive_nodes.len(),
        5,
        "the recursive team's start-triggered root node plus its four escalated Plan \
         descendants must all appear in the graph; graph: {graph:#?}"
    );
    for node in &recursive_nodes {
        assert_eq!(
            node.kind,
            NodeKind::Plan,
            "every node in the recursion chain is a Plan node; graph: {graph:#?}"
        );
        assert_eq!(
            node.status,
            NodeStatus::Completed,
            "every node in the recursion chain must have completed; graph: {graph:#?}"
        );
        assert_eq!(
            node.adapter,
            recursive_adapter_path.to_string_lossy(),
            "node {:?} lost the recursive team's adapter somewhere in the recursion; \
             graph: {graph:#?}",
            node.id
        );
        assert_eq!(
            node.northstar,
            recursive_northstar_path.to_string_lossy(),
            "node {:?} lost the recursive team's northstar somewhere in the recursion; \
             graph: {graph:#?}",
            node.id
        );
    }

    // No node anywhere in the graph — including the root node — silently
    // fell back to the top-level/default adapter's empty team/adapter/
    // northstar.
    let non_recursive: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team != "recursive")
        .collect();
    assert!(
        non_recursive.is_empty(),
        "every node, including the root bootstrap node, must belong to the recursive \
         team once it is the run's sole start-triggered team; graph: {graph:#?}"
    );

    // The manifest committed to the artifact records the leaf tasks under
    // the recursive team, proving each branch's `"kind":"task"` output was
    // integrated with the team it actually ran under.
    let final_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let manifest = git_output(
        &repo_path,
        &["show", &format!("{final_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let manifest_tasks = manifest["tasks"]
        .as_array()
        .expect("manifest tasks must be an array");
    for task_id in ["leaf-b", "leaf-a1-task", "leaf-a2-task"] {
        let task = manifest_tasks
            .iter()
            .find(|t| t["id"] == task_id)
            .unwrap_or_else(|| panic!("manifest must have a row for {task_id}"));
        assert_eq!(
            task["team"], "recursive",
            "{task_id}'s manifest row must record the recursive team, not the root's; \
             manifest: {manifest:#?}"
        );
    }

    let _ = handler; // handler retained only to keep the artifact alive for the git reads above
}
