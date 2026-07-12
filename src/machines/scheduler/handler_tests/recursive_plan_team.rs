use super::*;

use std::collections::VecDeque;
use std::path::Path;

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
/// order. Identical in shape to `multi_team::RecordingScriptedProvider`.
struct RecordingScriptedProvider {
    prompts: RefCell<Vec<String>>,
    responses: RefCell<VecDeque<String>>,
}

impl RecordingScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            prompts: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
        }
    }
}

impl ProviderClient for RecordingScriptedProvider {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.prompts.borrow_mut().push(request.prompt.clone());
        let content = self.responses.borrow_mut().pop_front().unwrap_or_else(|| {
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
/// (`trigger: start`), whose adapter's planner recurses through two levels of
/// `"kind":"plan"` output before finally emitting `"kind":"task"`, driven
/// through the real `ForgeConfig::from_file` -> `SchedulerMachine` ->
/// `SchedulerHandler` -> `DeliberatingNodeRunner` stack, with a scripted
/// provider standing in for the LLM.
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
/// Proves: every node in the `recursive` team's Plan -> Plan -> Plan chain —
/// not just the first — carries the team's own `team`/`adapter`/`northstar`,
/// not the root/default adapter's (empty) fields.
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

    // Fixture adapters and northstars: a distinct root/default adapter plus
    // the `recursive` team's own, so a team-loss bug (children silently
    // falling back to the root adapter's empty team/adapter/northstar) would
    // be visible.
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

    // Wire the root/default adapter the same way `RunSession::drive` does.
    let root_setup = ProjectRuntimeSetup::build(Path::new(&config.adapter), None)
        .expect("root adapter must load");

    // Response order, matching how the scheduler will actually dispatch —
    // every Plan-kind node (whether it produces children or a task batch)
    // goes through one producer/critic/referee cycle:
    //   1. root Plan node (team "", falls back to the root adapter) emits a
    //      trivial `"kind":"task"` batch so it completes without spawning
    //      any children of its own.
    //   2. Once root's tasks are integrated, the `recursive` team's Start
    //      trigger spawns its own Plan node (depth 1), which emits
    //      `"kind":"plan"` with one task, escalating to a further Plan child.
    //   3. That Plan child (depth 2) itself emits `"kind":"plan"` with one
    //      task, escalating to a second Plan child.
    //   4. That grandchild Plan node (depth 3) finally emits `"kind":"task"`,
    //      recording a real task row to the manifest with no further
    //      children.
    let provider = RecordingScriptedProvider::from_strs(&[
        // 1. root Plan node
        r#"{"kind":"task","tasks":[{"id":"root-t1","objective":"decompose the objective","name":"root_t1","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"root plan ok"}"#,
        r#"{"status":"accepted","content":"root plan approved"}"#,
        // 2. recursive team's own Plan node (depth 1)
        r#"{"kind":"plan","tasks":[{"id":"plan-depth-2","objective":"decompose further","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"depth 1 critic ok"}"#,
        r#"{"status":"accepted","content":"depth 1 referee approved"}"#,
        // 3. depth-2 Plan child
        r#"{"kind":"plan","tasks":[{"id":"plan-depth-3","objective":"decompose once more","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"depth 2 critic ok"}"#,
        r#"{"status":"accepted","content":"depth 2 referee approved"}"#,
        // 4. depth-3 Plan grandchild: finally produces a task batch
        r#"{"kind":"task","tasks":[{"id":"leaf-task","objective":"do the leaf work","name":"leaf_task","depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"depth 3 critic ok"}"#,
        r#"{"status":"accepted","content":"depth 3 referee approved"}"#,
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
        },
    );

    let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, &telemetry);

    let SchedulerTerminalOutput::Complete { graph, .. } = output else {
        panic!("expected the run to complete, got: {output:#?}");
    };

    // The whole Plan -> Plan -> Plan chain was actually spawned under the
    // "recursive" team: not just the team's own first node, but its child
    // and grandchild as well.
    let recursive_nodes: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team == "recursive")
        .collect();
    assert_eq!(
        recursive_nodes.len(),
        3,
        "the recursive team's start-triggered node plus its two escalated Plan \
         children must all appear in the graph; graph: {graph:#?}"
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

    // No node anywhere in the graph silently fell back to the root
    // adapter's empty team/adapter/northstar except the root node itself.
    let non_root_non_recursive: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.team != "recursive" && n.origin != NodeOrigin::Root)
        .collect();
    assert!(
        non_root_non_recursive.is_empty(),
        "every non-root node must belong to the recursive team; graph: {graph:#?}"
    );

    // The manifest committed to the artifact records the leaf task under the
    // recursive team, proving depth-3's `"kind":"task"` output was integrated
    // with the team it actually ran under, not the root's.
    let final_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let manifest = git_output(
        &repo_path,
        &["show", &format!("{final_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let leaf_task = manifest["tasks"]
        .as_array()
        .expect("manifest tasks must be an array")
        .iter()
        .find(|t| t["id"] == "leaf-task")
        .expect("manifest must have a row for the leaf task");
    assert_eq!(
        leaf_task["team"], "recursive",
        "the leaf task's manifest row must record the recursive team, not the root's; \
         manifest: {manifest:#?}"
    );

    let _ = handler; // handler retained only to keep the artifact alive for the git reads above
}
