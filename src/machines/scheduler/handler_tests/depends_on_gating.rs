use super::*;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use crate::config::ForgeConfig;
use crate::machines::scheduler::run_scheduler_with_telemetry;
use crate::node_runner::ProjectRuntimeSetup;
use crate::providers::{ProviderClient, ProviderError, ProviderRequest, ProviderResponse};

/// Same trivial one-planner/one-worker adapter shape as
/// `multi_team::adapter_yaml`, duplicated locally (per this test module's
/// established convention of not sharing fixture builders across
/// `handler_tests` files) so this file stays self-contained.
fn adapter_yaml(marker: &str, plugins: &[String]) -> String {
    let plugins_yaml = if plugins.is_empty() {
        "[]".to_string()
    } else {
        let entries = plugins
            .iter()
            .map(|p| format!("  - \"{p}\""))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n{entries}")
    };
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
workers:
  - plugin_role: implementer
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
plugins: {plugins_yaml}
"#
    )
}

/// A language plugin whose `name_target_rules` derives a distinct target
/// file per task name (`{name}` -> `{name}.txt`), so `task_one` and
/// `task_two` each get their own file instead of colliding on one target.
fn plugin_yaml() -> String {
    r#"
extensions: ["txt"]
init:
  commands: []
validation:
  runs_tests: false
  commands: []
plugin_roles:
  - plugin_role: implementer
    validation:
      runs_tests: false
      commands: []
name_target_rules:
  - pattern: "{name}"
    target: "{name}.txt"
"#
    .to_string()
}

/// Records every prompt it is called with and replays scripted responses in
/// order. Identical to `multi_team::RecordingScriptedProvider`; `Mutex`
/// (rather than `RefCell`) is required only so the type is `Sync`, as the
/// scheduler driver shares `&NodeRunner` across dispatch threads. Both
/// tests here run with `dispatch_cap: 1`, so at most one node is ever in
/// flight and the scripted queue is still consumed in a fixed order.
struct RecordingScriptedProvider {
    prompts: Mutex<Vec<String>>,
    responses: Mutex<VecDeque<String>>,
}

impl RecordingScriptedProvider {
    fn from_strs(responses: &[String]) -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.iter().cloned().collect()),
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

/// Corrupts the artifact's bare-repository invariant on every validation
/// pass, so `integrate()`'s `check_bare_repository` guard fails
/// deterministically and permanently. Unlike a CAS conflict (which is
/// retryable and self-heals on the next attempt), this failure persists
/// across retries, making it suitable for exercising a genuinely terminal
/// integration failure.
struct NonBareRepoValidator {
    repo_path: PathBuf,
}

impl Validator for NonBareRepoValidator {
    fn validate(&self, _workspace: &Workspace) -> ValidationResult {
        crate::git::command()
            .args(["config", "core.bare", "false"])
            .current_dir(&self.repo_path)
            .status()
            .expect("failed to corrupt bare repository invariant");
        ValidationResult {
            passed: true,
            summary: "corrupted the bare repository invariant to force a terminal integration \
                      failure"
                .to_string(),
            failure: None,
        }
    }
}

/// A two-team (`planner` start-triggered, `worker` after_teams(planner))
/// `forge.yaml` fixture, matching `multi_team.rs`'s shape.
struct Fixture {
    _temp: TempDirectory,
    repo_path: PathBuf,
    artifact: Artifact,
    config: ForgeConfig,
    root_setup: ProjectRuntimeSetup,
}

fn build_fixture(label: &str) -> Fixture {
    let temp = TempDirectory::new(label);

    let seed_path = temp.join("seed");
    fs::create_dir(&seed_path).expect("failed to create seed directory");
    git(&seed_path, &["init", "--quiet", "--initial-branch=main"]);
    git(
        &seed_path,
        &["config", "user.name", "Depends-On Gating Test"],
    );
    git(
        &seed_path,
        &["config", "user.email", "depends-on-test@example.invalid"],
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

    let worker_plugin_path = temp.join("worker_plugin.yaml");
    fs::write(&worker_plugin_path, plugin_yaml()).expect("write worker plugin");

    let root_adapter_path = temp.join("root_adapter.yaml");
    fs::write(&root_adapter_path, adapter_yaml("ROOT", &[])).expect("write root adapter");
    let planner_adapter_path = temp.join("planner_adapter.yaml");
    fs::write(&planner_adapter_path, adapter_yaml("PLANNER-TEAM", &[]))
        .expect("write planner adapter");
    let worker_adapter_path = temp.join("worker_adapter.yaml");
    fs::write(
        &worker_adapter_path,
        adapter_yaml(
            "WORKER-TEAM",
            &[worker_plugin_path.to_string_lossy().into_owned()],
        ),
    )
    .expect("write worker adapter");

    let planner_northstar_path = temp.join("planner_northstar.txt");
    fs::write(
        &planner_northstar_path,
        "PLANNER-TEAM-NORTHSTAR: decompose into a dependent worker task pair.\n",
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
objective: "Ship a two-task dependency chain fixture."
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

    let config = ForgeConfig::from_file(&forge_yaml_path.to_string_lossy())
        .expect("forge.yaml fixture must load and validate");
    assert_eq!(config.teams.len(), 2);
    assert_eq!(
        config.terminal_teams,
        vec!["worker".to_string()],
        "worker must be the sole terminal team: planner is referenced by worker's \
         after_teams, so depends_on gating is checked against worker completions"
    );

    let root_setup = ProjectRuntimeSetup::build(Path::new(&config.adapter), None, &config.language)
        .expect("root adapter must load");

    Fixture {
        _temp: temp,
        repo_path,
        artifact,
        config,
        root_setup,
    }
}

/// The scripted responses for the root Plan node, which *is* the `planner`
/// team's `trigger: start` node (seeded with `planner`'s own
/// team/adapter/northstar, since `planner` is this fixture's sole
/// `Trigger::Start` team). It emits two tasks directly — `task-1`
/// (`task_one`, no dependencies) and `task-2` (`task_two`, `depends_on:
/// ["task-1"]`).
fn root_and_planner_responses() -> Vec<String> {
    [
        r#"{"kind":"task","tasks":[{"id":"task-1","objective":"implement the first task","name":"task_one","depends_on":[]},{"id":"task-2","objective":"implement the second task, which depends on the first","name":"task_two","depends_on":["task-1"]}]}"#,
        r#"{"status":"accepted","content":"planner critic ok"}"#,
        r#"{"status":"accepted","content":"planner referee approved"}"#,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// A full producer/critic/referee tool-calling cycle for one `Work` node:
/// write the target file, then two read-back rounds (critic, referee), both
/// approved. Matches the shape `multi_team.rs` uses for its worker node.
fn work_cycle_responses(target: &str) -> Vec<String> {
    vec![
        format!(r#"{{"tool":"write_file","path":"{target}","content":"done\n"}}"#),
        format!(r#"{{"summary":"finished writing {target}"}}"#),
        format!(r#"{{"tool":"read_file","path":"{target}"}}"#),
        r#"{"status":"accepted","content":"worker critic ok"}"#.to_string(),
        format!(r#"{{"tool":"read_file","path":"{target}"}}"#),
        r#"{"status":"accepted","content":"worker referee approved"}"#.to_string(),
    ]
}

/// Proves the negative half of `depends_on` gating: `task-2` is a `ForTasks`
/// candidate the moment the `planner` team's Plan node integrates (both
/// tasks get a `planner` completion row in the same batch), but it must not
/// spawn a `worker` node until `task-1` has a completion row from every
/// terminal team. Here `task-1`'s `worker` node is made to fail its
/// integration permanently (a genuine terminal git-level error, injected via
/// `NonBareRepoValidator` — deliberately not a CAS conflict, which is
/// retryable and would let task-1 eventually succeed), so `task-1`'s
/// dependency is never satisfied and the whole run halts. If gating were broken (both ids spawned
/// eagerly, ungated), `task-2`'s node would already exist in the graph by
/// the time the run halts; correct gating means it must never have been
/// created at all.
#[test]
fn second_task_never_spawns_while_its_dependency_never_completes() {
    let fixture = build_fixture("depends-on-gating-blocked");

    let mut responses = root_and_planner_responses();
    responses.extend(work_cycle_responses("task_one.txt"));
    let provider = RecordingScriptedProvider::from_strs(&responses);

    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_role_policy(fixture.root_setup.role_policy)
        .with_required_test_targets_fn(fixture.root_setup.required_test_targets_fn)
        .with_context_file_names(fixture.root_setup.context_file_names)
        .with_api_summary_command(fixture.root_setup.api_summary_command)
        .with_language_plugins(fixture.root_setup.language_plugins)
        .with_validation_plan_for_role_fn(fixture.root_setup.validation_plan_for_role_fn);

    let handler = SchedulerHandler::with_artifact(runner, fixture.artifact).with_validator(
        Arc::new(NonBareRepoValidator {
            repo_path: fixture.repo_path.clone(),
        }),
    );
    let telemetry = VecTelemetry::new();

    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: fixture.config.root_objective().to_string(),
        },
        RunConfig {
            has_strong_tier: false,
            teams: fixture.config.teams.clone(),
            terminal_teams: fixture.config.terminal_teams.clone(),
            dispatch_cap: 1,
        },
    );

    let (output, _handler) = run_scheduler_with_telemetry(handler, initial_state, &telemetry);

    let SchedulerTerminalOutput::Failed { graph, reason } = output else {
        panic!(
            "expected the run to halt on task-1's forced terminal integration failure, got: {output:#?}"
        );
    };
    assert!(
        matches!(reason, FailureReason::TerminalRecovery { .. }),
        "task-1's integration must fail via a Terminal recovery action (the only kind a \
         real git integrate() error produces); got: {reason:#?}"
    );

    let worker_nodes: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "worker").collect();
    assert_eq!(
        worker_nodes.len(),
        1,
        "task-2 must never have been spawned while task-1 (its dependency) never reached \
         a completion row for the terminal team; graph: {graph:#?}"
    );
    assert_eq!(worker_nodes[0].task_id.as_deref(), Some("task-1"));
    assert_eq!(worker_nodes[0].status, NodeStatus::Failed);
}

/// Proves the positive half of `depends_on` gating: once `task-1` actually
/// completes (records a `worker` completion row), the `worker` team's
/// `after_teams(planner)` trigger re-evaluates and `task-2` becomes eligible,
/// spawning its own `Work` node — driven end-to-end through the real
/// scheduler/dispatch loop, not constructed directly against `TaskRecord`s.
#[test]
fn second_task_spawns_once_its_dependency_completes_for_every_terminal_team() {
    let fixture = build_fixture("depends-on-gating-happy");
    let repo_path = fixture.repo_path.clone();

    let mut responses = root_and_planner_responses();
    responses.extend(work_cycle_responses("task_one.txt"));
    responses.extend(work_cycle_responses("task_two.txt"));
    let provider = RecordingScriptedProvider::from_strs(&responses);

    let runner = DeliberatingNodeRunner::new(&provider, &provider)
        .with_role_policy(fixture.root_setup.role_policy)
        .with_required_test_targets_fn(fixture.root_setup.required_test_targets_fn)
        .with_context_file_names(fixture.root_setup.context_file_names)
        .with_api_summary_command(fixture.root_setup.api_summary_command)
        .with_language_plugins(fixture.root_setup.language_plugins)
        .with_validation_plan_for_role_fn(fixture.root_setup.validation_plan_for_role_fn);

    let handler = SchedulerHandler::with_artifact(runner, fixture.artifact);
    let telemetry = VecTelemetry::new();

    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: fixture.config.root_objective().to_string(),
        },
        RunConfig {
            has_strong_tier: false,
            teams: fixture.config.teams.clone(),
            terminal_teams: fixture.config.terminal_teams.clone(),
            dispatch_cap: 1,
        },
    );

    let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, &telemetry);

    let SchedulerTerminalOutput::Complete {
        graph,
        recovery_summary,
    } = output
    else {
        panic!("expected the run to complete, got: {output:#?}");
    };
    assert!(
        !recovery_summary.recovered,
        "this run is scripted to succeed cleanly, with no retries or escalations; \
         recovery_summary: {recovery_summary:#?}"
    );

    let worker_nodes: Vec<&Node> = graph.nodes.iter().filter(|n| n.team == "worker").collect();
    assert_eq!(
        worker_nodes.len(),
        2,
        "both task-1 and task-2 must have spawned worker nodes once task-1 completed; \
         graph: {graph:#?}"
    );
    for node in &worker_nodes {
        assert_eq!(
            node.status,
            NodeStatus::Completed,
            "node {:?} must have completed; graph: {graph:#?}",
            node.task_id
        );
    }
    let task_ids: Vec<&str> = worker_nodes
        .iter()
        .filter_map(|n| n.task_id.as_deref())
        .collect();
    assert!(task_ids.contains(&"task-1"));
    assert!(task_ids.contains(&"task-2"));

    // The manifest committed to the artifact preserves task-2's depends_on
    // on its planner-recorded row (the row `retain_ids_with_satisfied_dependencies`
    // reads), and both task-1 and task-2 additionally gained a "worker"
    // completion row once their Work nodes integrated.
    let final_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let manifest = git_output(
        &repo_path,
        &["show", &format!("{final_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let tasks = manifest["tasks"]
        .as_array()
        .expect("tasks must be an array");
    let planner_task_2 = tasks
        .iter()
        .find(|t| t["id"] == "task-2" && t["team"] == "planner")
        .expect("manifest must have a planner-recorded row for task-2");
    assert_eq!(
        planner_task_2["depends_on"],
        serde_json::json!(["task-1"]),
        "task-2's planner-recorded row must preserve its depends_on; manifest: {manifest:#?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| t["id"] == "task-1" && t["team"] == "worker"),
        "manifest must have a worker completion row for task-1; manifest: {manifest:#?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| t["id"] == "task-2" && t["team"] == "worker"),
        "manifest must have a worker completion row for task-2; manifest: {manifest:#?}"
    );

    let _ = handler; // retained only to keep the artifact alive for the git reads above
}
