use super::*;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use crate::config::ForgeConfig;
use crate::machines::scheduler::run_scheduler_with_telemetry;
use crate::node_runner::ProjectRuntimeSetup;
use crate::providers::{ProviderClient, ProviderError, ProviderRequest, ProviderResponse};

/// A trivial adapter YAML: one planner + one worker role, with `marker`
/// baked into the planner producer's identity so the rendered prompt proves
/// which adapter a node actually ran under.
fn adapter_yaml(marker: &str) -> String {
    format!(
        r#"
planner:
  producer:
    identity: "{marker}: planner producer"
    context: ""
    instructions: "Emit {{\"kind\":\"task\",\"tasks\":[...]}}."
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
  provides: [name, function_name, file_path]
workers:
  - key: implementer
    requires: [file_path]
    description: "Implements assigned tasks."
    producer:
      identity: "{marker}: worker producer"
      context: ""
      instructions: ""
      constraints: ""
    critic:
      identity: "{marker}: worker critic"
      context: ""
      instructions: ""
      constraints: ""
    referee:
      identity: "{marker}: worker referee"
      context: ""
      instructions: ""
      constraints: ""
context_files: []
plugins: []
"#
    )
}

/// Records every prompt it is called with (so tests can assert which
/// adapter's marker text reached the model) and replays scripted responses
/// in order.
///
/// This single-team-at-a-time fixture's graph has a strict dependency chain
/// (root plan -> planner plan -> worker work), so even under concurrent
/// dispatch at most one node is ever in flight — the scripted response
/// queue is still consumed in a fixed, predictable order. `Mutex` (rather
/// than `RefCell`) is required only so the type is `Sync`, as the scheduler
/// driver shares `&NodeRunner` across dispatch threads.
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

    fn recorded_prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("mutex poisoned").clone()
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

/// End-to-end: a `forge.yaml` fixture with a `planner` team (`trigger:
/// start`) and a `worker` team (`trigger: after_teams(planner)`), each with
/// its own adapter and northstar, driven through the real
/// `ForgeConfig::from_file` -> `SchedulerMachine` -> `SchedulerHandler` ->
/// `DeliberatingNodeRunner` stack (the same wiring `RunSession::drive` uses),
/// with a scripted provider standing in for the LLM.
///
/// Proves: the root node *is* the `planner` team's `trigger: start` node
/// (seeded with `planner`'s own team/adapter/northstar, since `planner` is
/// the run's sole `Trigger::Start` team) rather than a separate blank-team
/// bootstrap node; once its tasks land in the manifest correctly attributed
/// to `planner`, the `worker` team's `after_teams(planner)` trigger spawns a
/// Work node for the resulting task; and each spawned node's rendered prompt
/// carries its own team's adapter marker (not the top-level/default
/// adapter's, and not the other team's).
#[test]
fn two_team_forge_yaml_drives_planner_then_worker_under_their_own_adapters() {
    let temp = TempDirectory::new("multi-team-e2e");

    // Seed a real bare artifact repo, exactly as `load_or_create_artifact`
    // expects to find (or as `fixture()` builds for other handler tests).
    let seed_path = temp.join("seed");
    fs::create_dir(&seed_path).expect("failed to create seed directory");
    git(&seed_path, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed_path, &["config", "user.name", "Multi Team Test"]);
    git(
        &seed_path,
        &["config", "user.email", "multi-team-test@example.invalid"],
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

    // Fixture adapters and northstars, one set per team plus a distinct
    // top-level/root one, so a marker mismatch would be visible.
    let root_adapter_path = temp.join("root_adapter.yaml");
    fs::write(&root_adapter_path, adapter_yaml("ROOT")).expect("write root adapter");
    let planner_adapter_path = temp.join("planner_adapter.yaml");
    fs::write(&planner_adapter_path, adapter_yaml("PLANNER-TEAM")).expect("write planner adapter");
    let worker_adapter_path = temp.join("worker_adapter.yaml");
    fs::write(&worker_adapter_path, adapter_yaml("WORKER-TEAM")).expect("write worker adapter");

    let planner_northstar_path = temp.join("planner_northstar.txt");
    fs::write(
        &planner_northstar_path,
        "PLANNER-TEAM-NORTHSTAR: decompose into worker tasks.\n",
    )
    .expect("write planner northstar");
    let worker_northstar_path = temp.join("worker_northstar.txt");
    fs::write(
        &worker_northstar_path,
        "WORKER-TEAM-NORTHSTAR: implement assigned tasks.\n",
    )
    .expect("write worker northstar");

    let forge_yaml = format!(
        r#"
objective: "Ship a trivial two-team fixture."
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
language: "txt"
teams:
  - name: planner
    northstar: "{planner_northstar}"
    adapter: "{planner_adapter}"
    kind: plan
    trigger: start
  - name: worker
    northstar: "{worker_northstar}"
    adapter: "{worker_adapter}"
    kind: work
    trigger: after_teams(planner)
"#,
        repo_path = repo_path.display(),
        telemetry_dir = temp.join("telemetry").display(),
        root_adapter = root_adapter_path.display(),
        planner_northstar = planner_northstar_path.display(),
        planner_adapter = planner_adapter_path.display(),
        worker_northstar = worker_northstar_path.display(),
        worker_adapter = worker_adapter_path.display(),
    );
    let forge_yaml_path = temp.join("forge.yaml");
    fs::write(&forge_yaml_path, forge_yaml).expect("write forge.yaml");

    // Load exactly the way `forge start` does: `ForgeConfig::from_file`
    // resolves and validates every team's adapter/northstar up front.
    let config = ForgeConfig::from_file(&forge_yaml_path.to_string_lossy())
        .expect("forge.yaml fixture must load and validate");
    assert_eq!(config.teams.len(), 2);

    // Wire the root/default adapter the same way `RunSession::drive` does.
    let root_setup = ProjectRuntimeSetup::build(Path::new(&config.adapter), None, &config.language)
        .expect("root adapter must load");

    // Response order, matching how the scheduler will actually dispatch:
    //   1. The root Plan node *is* the `planner` team's `trigger: start`
    //      node (its team/adapter/northstar are seeded from `planner` at
    //      `SchedulerMachine::initial_state`, since `planner` is the run's
    //      sole `Trigger::Start` team), so it runs under the planner
    //      adapter/northstar and emits its task batch directly — no
    //      separate throwaway root pass.
    //   2. Once the planner's task lands in the manifest with team
    //      "planner", the `worker` team's `after_teams(planner)` trigger
    //      spawns a Work node for it. Because the run has a real artifact,
    //      the Work node goes through the tool-calling producer/critic/
    //      referee loop (write_file, then read_file twice).
    let provider = RecordingScriptedProvider::from_strs(&[
        // 1. root/planner-team Plan node
        r#"{"kind":"task","tasks":[{"id":"task-1","objective":"implement the worker task","task_kv":{"name":"worker_task","function_name":"worker_task","file_path":"worker_output.txt"},"depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"planner critic ok"}"#,
        r#"{"status":"accepted","content":"planner referee approved"}"#,
        // 2. worker-team Work node
        r#"{"tool":"write_file","path":"worker_output.txt","content":"done by worker team\n"}"#,
        r#"{"summary":"worker team finished task-1"}"#,
        r#"{"tool":"read_file","path":"worker_output.txt"}"#,
        r#"{"status":"accepted","content":"worker critic ok"}"#,
        r#"{"tool":"read_file","path":"worker_output.txt"}"#,
        r#"{"status":"accepted","content":"worker referee approved"}"#,
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

    // The trigger actually fired: a Plan node was spawned under "planner"
    // and a Work node was spawned under "worker".
    let planner_nodes: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "planner").collect();
    let worker_nodes: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "worker").collect();
    assert_eq!(
        planner_nodes.len(),
        1,
        "planner team's start trigger must spawn exactly one node; graph: {graph:#?}"
    );
    assert_eq!(
        worker_nodes.len(),
        1,
        "worker team's after_teams(planner) trigger must spawn exactly one node; graph: {graph:#?}"
    );
    assert_eq!(planner_nodes[0].kind, NodeKind::Plan);
    assert_eq!(worker_nodes[0].kind, NodeKind::Work);
    assert_eq!(worker_nodes[0].status, NodeStatus::Completed);
    assert_eq!(planner_nodes[0].status, NodeStatus::Completed);

    // Each spawned node ran under its own team's adapter/northstar, not the
    // root/default adapter's and not the other team's — proven by which
    // marker string reached the provider.
    let prompts = provider.recorded_prompts();
    let planner_prompt = &prompts[0]; // root/planner-team Plan node's producer call
    assert!(
        planner_prompt.contains("PLANNER-TEAM: planner producer"),
        "planner node's prompt must carry the planner team's adapter marker; got:\n{planner_prompt}"
    );
    assert!(
        planner_prompt.contains("PLANNER-TEAM-NORTHSTAR"),
        "planner node's prompt must carry the planner team's northstar; got:\n{planner_prompt}"
    );
    assert!(
        !planner_prompt.contains("ROOT:") && !planner_prompt.contains("WORKER-TEAM"),
        "planner node's prompt must not leak the root/default or worker team's wiring; got:\n{planner_prompt}"
    );

    // Northstar is surfaced only to Plan-node prompts by design (see
    // `node_runner::deliberating::context`), so the Work node's prompt is
    // checked for the adapter marker only, not the northstar text.
    let worker_prompt = &prompts[3]; // worker-team Work node's producer call
    assert!(
        worker_prompt.contains("WORKER-TEAM: worker producer"),
        "worker node's prompt must carry the worker team's adapter marker; got:\n{worker_prompt}"
    );
    assert!(
        !worker_prompt.contains("ROOT:") && !worker_prompt.contains("PLANNER-TEAM:"),
        "worker node's prompt must not leak the root or planner team's wiring; got:\n{worker_prompt}"
    );

    // The manifest committed to the artifact actually records both teams'
    // work, keyed by team name.
    let final_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let manifest = git_output(
        &repo_path,
        &["show", &format!("{final_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let teams_in_manifest: Vec<&str> = manifest["tasks"]
        .as_array()
        .expect("manifest tasks must be an array")
        .iter()
        .filter_map(|t| t["team"].as_str())
        .collect();
    assert!(
        teams_in_manifest.contains(&"planner"),
        "manifest must record a row for the planner team; got: {teams_in_manifest:?}"
    );
    assert!(
        teams_in_manifest.contains(&"worker"),
        "manifest must record a row for the worker team; got: {teams_in_manifest:?}"
    );

    let _ = handler; // handler retained only to keep the artifact alive for the git reads above
}
