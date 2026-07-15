use super::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Creates a fresh, uniquely named directory, so each `TempYaml` gets its
/// own isolated config directory instead of sharing one process-wide temp
/// directory (tests that write companion files next to the config would
/// otherwise race with each other under parallel test execution).
fn unique_config_dir() -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("forge-config-test-{}-{}", std::process::id(), id,));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Copies a built-in adapter/plugin YAML from this crate's `adapters/`,
/// `testdata/`, or `plugins/` directory into `dir` (as `name`), so config
/// fixtures that reference it by a bare relative filename (e.g.
/// `adapter: coding.yaml`) resolve correctly against the temp directory
/// holding the config file. A no-op if already staged.
///
/// Adapter content is staged flat alongside its plugins (not nested under
/// `adapters/`/`testdata/`/`plugins/` subdirectories), so the shipped
/// adapters' `../plugins/...` plugin paths are rewritten to bare filenames
/// to match — otherwise they'd resolve outside the isolated temp dir.
fn stage_fixture(dir: &std::path::Path, subdir: &str, name: &str) {
    let dest = dir.join(name);
    if dest.exists() {
        return;
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(subdir)
        .join(name);
    let content = std::fs::read_to_string(src).unwrap();
    let content = content.replace("../plugins/", "");
    std::fs::write(dest, content).unwrap();
}

struct TempYaml(PathBuf);

impl TempYaml {
    fn new(content: &str) -> Self {
        let dir = unique_config_dir();
        let path = dir.join("config.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        stage_fixture(&dir, "testdata", "coding.yaml");
        for name in [
            "planner.yaml",
            "implement.yaml",
            "create_test.yaml",
            "pass_tests.yaml",
        ] {
            stage_fixture(&dir, "adapters", name);
        }
        for name in ["rust.yaml", "python.yaml"] {
            stage_fixture(&dir, "plugins", name);
        }
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }

    fn dir(&self) -> &std::path::Path {
        self.0.parent().unwrap()
    }
}

impl Drop for TempYaml {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

const EXAMPLE_YAML: &str = r#"
objective: "Write a short haiku about Rust state machines."
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn parses_objective() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.objective.as_deref(),
        Some("Write a short haiku about Rust state machines.")
    );
    assert_eq!(
        config.root_objective(),
        "Write a short haiku about Rust state machines."
    );
}

const NO_OBJECTIVE_YAML: &str = r#"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn objective_is_required() {
    let tmp = TempYaml::new(NO_OBJECTIVE_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("objective is required"),
        "error must explain that objective is required; got: {err}"
    );
}

// ── teams config tests ───────────────────────────────────────────────────

const TEAMS_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: project.md
    adapter: coding.yaml
    kind: plan
    trigger: start
  - name: implement
    northstar: implementation.md
    adapter: coding.yaml
    kind: work
    trigger: after_teams(planner)
"#;

#[test]
fn parses_teams() {
    let tmp = TempYaml::new(TEAMS_YAML);
    std::fs::write(tmp.dir().join("project.md"), "gap: project").unwrap();
    std::fs::write(tmp.dir().join("implementation.md"), "gap: implementation").unwrap();
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(config.teams.len(), 2);

    let expected_adapter = tmp.dir().join("coding.yaml").to_string_lossy().into_owned();

    let planner = &config.teams[0];
    assert_eq!(planner.name, "planner");
    assert_eq!(
        planner.northstar,
        tmp.dir().join("project.md").to_string_lossy()
    );
    assert_eq!(planner.adapter, expected_adapter);
    assert_eq!(planner.kind, NodeKind::Plan);
    assert_eq!(planner.trigger, Trigger::Start);

    let implement = &config.teams[1];
    assert_eq!(implement.name, "implement");
    assert_eq!(
        implement.northstar,
        tmp.dir().join("implementation.md").to_string_lossy()
    );
    assert_eq!(implement.adapter, expected_adapter);
    assert_eq!(implement.kind, NodeKind::Work);
    assert_eq!(
        implement.trigger,
        Trigger::AfterTeams(vec!["planner".to_string()])
    );
}

#[test]
fn team_name_target_rules_are_merged_from_its_adapter_language_plugins() {
    // Invariant: each team's name_target_rules is populated at config-load
    // time from the language plugins its own adapter declares (coding.yaml
    // declares both python.yaml and rust.yaml), not left empty — this is
    // what lets a ForTasks-spawned node derive target_files from a task
    // name with no I/O inside the (pure) scheduler transition.
    let tmp = TempYaml::new(TEAMS_YAML);
    std::fs::write(tmp.dir().join("project.md"), "gap: project").unwrap();
    std::fs::write(tmp.dir().join("implementation.md"), "gap: implementation").unwrap();
    let config = ForgeConfig::from_file(tmp.path()).unwrap();

    for team in &config.teams {
        let targets: Vec<&str> = team
            .name_target_rules
            .iter()
            .map(|rule| rule.target.as_str())
            .collect();
        assert!(
            targets.contains(&"src/{name}.py") && targets.contains(&"src/{name}.rs"),
            "team '{}' must inherit name_target_rules from both plugins its adapter declares, got {targets:?}",
            team.name
        );
    }
}

#[test]
fn terminal_teams_populated_from_teams_at_load_time() {
    // Invariant: `ForgeConfig::from_file` wires `teams` through
    // `team_triggers::compute_terminal_teams` (detailed chain/branch/cycle
    // coverage lives in `config::team_triggers`'s own tests, since the
    // computation itself is pure and needs no adapter/northstar fixtures) —
    // this just confirms the field is actually populated, not left empty.
    let tmp = TempYaml::new(TEAMS_YAML);
    std::fs::write(tmp.dir().join("project.md"), "gap: project").unwrap();
    std::fs::write(tmp.dir().join("implementation.md"), "gap: implementation").unwrap();
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(config.terminal_teams, vec!["implement".to_string()]);
}

const TEAM_TRIGGER_CYCLE_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: a
    northstar: a.md
    adapter: coding.yaml
    kind: work
    trigger: after_teams(b)
  - name: b
    northstar: b.md
    adapter: coding.yaml
    kind: work
    trigger: after_teams(a)
"#;

#[test]
fn team_trigger_cycle_fails_to_load() {
    // Invariant: a team-trigger cycle (a's after_teams chain transitively
    // refers back to a) must fail config load loudly, not silently produce a
    // team that can never be scheduled.
    let tmp = TempYaml::new(TEAM_TRIGGER_CYCLE_YAML);
    for name in ["a", "b"] {
        std::fs::write(tmp.dir().join(format!("{name}.md")), "gap: x").unwrap();
    }
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("cycle"),
        "error must explain that a team trigger cycle was found; got: {err}"
    );
}

const MULTI_AFTER_EACH_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: gather
    northstar: gather.md
    adapter: coding.yaml
    kind: work
    trigger: after_teams(a, b)
"#;

#[test]
fn parses_after_teams_with_multiple_teams() {
    let tmp = TempYaml::new(MULTI_AFTER_EACH_YAML);
    std::fs::write(tmp.dir().join("gather.md"), "gap: gather").unwrap();
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.teams[0].trigger,
        Trigger::AfterTeams(vec!["a".to_string(), "b".to_string()])
    );
}

const MALFORMED_TRIGGER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: bogus
    northstar: northstars/bogus.md
    adapter: adapters/bogus.yaml
    kind: plan
    trigger: whenever()
"#;

#[test]
fn malformed_trigger_fails_fast_at_load() {
    let tmp = TempYaml::new(MALFORMED_TRIGGER_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("trigger must be"),
        "error must explain the trigger grammar; got: {err}"
    );
}

const KIND_PLAN_WITH_AFTER_EACH_TRIGGER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: project.md
    adapter: coding.yaml
    kind: plan
    trigger: after_teams(other)
"#;

#[test]
fn team_kind_plan_with_after_teams_trigger_fails_at_config_load_time() {
    // Invariant: `kind: plan` must pair with `trigger: start` — a team
    // declaring itself a planner but triggered `after_teams(...)` is a
    // config error, not something the scheduler should silently resolve by
    // trusting one field over the other.
    let tmp = TempYaml::new(KIND_PLAN_WITH_AFTER_EACH_TRIGGER_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("planner"),
        "error must name the mismatched team; got: {message}"
    );
    assert!(
        message.contains("plan") && message.contains("start"),
        "error must explain the kind/trigger mismatch; got: {message}"
    );
}

const KIND_WORK_WITH_START_TRIGGER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: worker
    northstar: project.md
    adapter: coding.yaml
    kind: work
    trigger: start
"#;

#[test]
fn team_kind_work_with_start_trigger_fails_at_config_load_time() {
    // Invariant: `kind: work` must pair with `trigger: after_teams(...)` —
    // the reverse mismatch of `team_kind_plan_with_after_teams_trigger_fails_at_config_load_time`.
    let tmp = TempYaml::new(KIND_WORK_WITH_START_TRIGGER_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("worker"),
        "error must name the mismatched team; got: {message}"
    );
    assert!(
        message.contains("work") && message.contains("after_teams"),
        "error must explain the kind/trigger mismatch; got: {message}"
    );
}

#[test]
fn teams_defaults_to_empty_when_absent() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert!(
        config.teams.is_empty(),
        "teams must default to empty when absent from the config"
    );
}

const BLANK_TEAM_ADAPTER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: project.md
    adapter: "   "
    kind: plan
    trigger: start
"#;

#[test]
fn team_adapter_is_required_when_blank() {
    // Invariant: a team's adapter must be validated the same way the
    // top-level adapter is (see `adapter_is_required_when_blank`), not left
    // to fail obscurely at dispatch time.
    let tmp = TempYaml::new(BLANK_TEAM_ADAPTER_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("planner"),
        "error must name the team with the blank adapter; got: {err}"
    );
}

const BLANK_TEAM_NORTHSTAR_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: "   "
    adapter: coding.yaml
    kind: plan
    trigger: start
"#;

#[test]
fn team_northstar_is_required_when_blank() {
    let tmp = TempYaml::new(BLANK_TEAM_NORTHSTAR_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("planner"),
        "error must name the team with the blank northstar; got: {err}"
    );
}

const UNKNOWN_TEAM_ADAPTER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: project.md
    adapter: bogus_team_adapter_that_does_not_exist.yaml
    kind: plan
    trigger: start
"#;

#[test]
fn unknown_team_adapter_filename_fails_at_config_load_time() {
    // Invariant: a team adapter path that does not exist on disk must fail
    // `from_file` itself, the same way an unknown top-level adapter does
    // (see `unknown_adapter_filename_fails_at_config_load_time`).
    let tmp = TempYaml::new(UNKNOWN_TEAM_ADAPTER_YAML);
    std::fs::write(tmp.dir().join("project.md"), "gap: project").unwrap();
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains("bogus_team_adapter_that_does_not_exist.yaml"),
        "error must name the missing team adapter path; got: {err}"
    );
}

const UNKNOWN_TEAM_NORTHSTAR_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: bogus_northstar_that_does_not_exist.md
    adapter: coding.yaml
    kind: plan
    trigger: start
"#;

#[test]
fn unknown_team_northstar_path_fails_at_config_load_time() {
    let tmp = TempYaml::new(UNKNOWN_TEAM_NORTHSTAR_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains("bogus_northstar_that_does_not_exist.md"),
        "error must name the missing team northstar path; got: {err}"
    );
}

/// A minimal team adapter declaring a worker role (`tester`) that the
/// paired plugin below deliberately omits from its `plugin_roles:` list.
const TEAM_ADAPTER_WITH_UNKNOWN_ROLE_YAML: &str = r#"
planner:
  producer:
    identity: "planner identity"
    context: "planner context"
    instructions: "planner instructions"
    constraints: "planner constraints"
  critic:
    identity: "critic identity"
    context: "critic context"
    instructions: "critic instructions"
    constraints: "critic constraints"
  referee:
    identity: "referee identity"
    context: "referee context"
    instructions: "referee instructions"
    constraints: "referee constraints"
workers:
  - plugin_role: tester
    description: "Writes tests."
    producer:
      identity: "test identity"
      context: "test context"
      instructions: "test instructions"
      constraints: "test constraints"
    critic:
      identity: "test critic identity"
      context: "test critic context"
      instructions: "test critic instructions"
      constraints: "test critic constraints"
    referee:
      identity: "test referee identity"
      context: "test referee context"
      instructions: "test referee instructions"
      constraints: "test referee constraints"
plugins:
  - broken_plugin.yaml
"#;

const PLUGIN_MISSING_TESTER_ROLE_YAML: &str = r#"
extensions: [zz]
init:
  commands: []
validation:
  commands: []
plugin_roles: []
"#;

const TEAM_WITH_UNKNOWN_WORKER_ROLE_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: project.md
    adapter: broken_team_adapter.yaml
    kind: plan
    trigger: start
"#;

#[test]
fn team_worker_role_missing_from_plugin_fails_at_config_load_time() {
    // Invariant: a team's adapter is validated for worker-role/plugin
    // coverage the same way the top-level adapter is (see
    // `adapter_role_missing_from_plugin_roles_fails_loudly` in
    // `project_setup_tests.rs`) — a role the team's adapter declares but
    // its plugin doesn't list must fail `from_file` itself, not surface
    // later at that team's first dispatch.
    let tmp = TempYaml::new(TEAM_WITH_UNKNOWN_WORKER_ROLE_YAML);
    std::fs::write(tmp.dir().join("project.md"), "gap: project").unwrap();
    std::fs::write(
        tmp.dir().join("broken_team_adapter.yaml"),
        TEAM_ADAPTER_WITH_UNKNOWN_ROLE_YAML,
    )
    .unwrap();
    std::fs::write(
        tmp.dir().join("broken_plugin.yaml"),
        PLUGIN_MISSING_TESTER_ROLE_YAML,
    )
    .unwrap();

    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("planner"),
        "error must name the team whose adapter has the unknown worker role; got: {message}"
    );
    assert!(
        message.contains("tester") && message.contains("zz"),
        "error must name the missing role and the plugin extension; got: {message}"
    );
}

/// A minimal team adapter declaring a plugin, but whose sole worker entry
/// omits `plugin_role` entirely — invalid, since the declared plugin has
/// nothing else to select this role's validation override by.
const TEAM_ADAPTER_WITH_MISSING_PLUGIN_ROLE_YAML: &str = r#"
planner:
  producer:
    identity: "planner identity"
    context: "planner context"
    instructions: "planner instructions"
    constraints: "planner constraints"
  critic:
    identity: "critic identity"
    context: "critic context"
    instructions: "critic instructions"
    constraints: "critic constraints"
  referee:
    identity: "referee identity"
    context: "referee context"
    instructions: "referee instructions"
    constraints: "referee constraints"
workers:
  - description: "Writes tests."
    producer:
      identity: "test identity"
      context: "test context"
      instructions: "test instructions"
      constraints: "test constraints"
    critic:
      identity: "test critic identity"
      context: "test critic context"
      instructions: "test critic instructions"
      constraints: "test critic constraints"
    referee:
      identity: "test referee identity"
      context: "test referee context"
      instructions: "test referee instructions"
      constraints: "test referee constraints"
plugins:
  - broken_plugin.yaml
"#;

const TEAM_WITH_MISSING_PLUGIN_ROLE_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
teams:
  - name: planner
    northstar: project.md
    adapter: broken_team_adapter.yaml
    kind: plan
    trigger: start
"#;

#[test]
fn team_worker_role_missing_plugin_role_fails_at_config_load_time() {
    // Invariant: a worker entry with no `plugin_role` is only valid when its
    // adapter declares no plugins at all (see
    // `plugin_role_defaults_to_none_when_omitted` in `yaml_config.rs`'s own
    // tests). Once a plugin is declared, every worker entry must name a
    // `plugin_role` — omitting it must fail `from_file` itself, naming the
    // team, not surface later at that team's first dispatch.
    let tmp = TempYaml::new(TEAM_WITH_MISSING_PLUGIN_ROLE_YAML);
    std::fs::write(tmp.dir().join("project.md"), "gap: project").unwrap();
    std::fs::write(
        tmp.dir().join("broken_team_adapter.yaml"),
        TEAM_ADAPTER_WITH_MISSING_PLUGIN_ROLE_YAML,
    )
    .unwrap();
    std::fs::write(
        tmp.dir().join("broken_plugin.yaml"),
        PLUGIN_MISSING_TESTER_ROLE_YAML,
    )
    .unwrap();

    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("planner"),
        "error must name the team whose adapter has the worker role missing plugin_role; got: {message}"
    );
    assert!(
        message.contains("plugin_role"),
        "error must mention the missing plugin_role; got: {message}"
    );
}

#[test]
fn parses_artifact_config() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let config_dir = std::path::Path::new(tmp.path()).parent().unwrap();
    let expected = config_dir
        .join(".forge/artifacts/main.git")
        .to_string_lossy()
        .into_owned();
    assert_eq!(config.artifact.repo_path, expected);
    assert_eq!(config.artifact.branch, "main");
}

#[test]
fn parses_provider_config() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let ProviderTierConfig::Unmanaged(cheap) = &config.provider.cheap else {
        panic!("cheap provider must parse as unmanaged");
    };
    assert_eq!(cheap.base_url, "http://localhost:8080");
    assert_eq!(cheap.model, "llama-test");
    assert_eq!(cheap.n_predict, 512);
}

#[test]
fn provider_timeout_defaults_reasonably() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.provider.timeout_seconds, 300,
        "absent timeout_seconds must default to 300"
    );
}

#[test]
fn dispatch_cap_defaults_to_four_when_absent() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.dispatch_cap, 4,
        "absent dispatch_cap must default to 4"
    );
}

const DISPATCH_CAP_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
dispatch_cap: 8
"#;

#[test]
fn parses_explicit_dispatch_cap() {
    let tmp = TempYaml::new(DISPATCH_CAP_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(config.dispatch_cap, 8);
}

const ZERO_DISPATCH_CAP_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
dispatch_cap: 0
"#;

#[test]
fn dispatch_cap_zero_fails_at_load_time() {
    let tmp = TempYaml::new(ZERO_DISPATCH_CAP_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("dispatch_cap"),
        "error must mention dispatch_cap; got: {err}"
    );
}

const PROVIDER_TIMEOUT_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
  timeout_seconds: 30
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn parses_explicit_provider_timeout() {
    let tmp = TempYaml::new(PROVIDER_TIMEOUT_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(config.provider.timeout_seconds, 30);
}

#[test]
fn parses_telemetry_config() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let config_dir = std::path::Path::new(tmp.path()).parent().unwrap();
    let expected = config_dir.join("runs").to_string_lossy().into_owned();
    assert_eq!(config.telemetry.directory, expected);
}

#[test]
fn missing_file_returns_error() {
    let result = ForgeConfig::from_file("/tmp/forge-nonexistent-config-test.yaml");
    assert!(result.is_err(), "missing file must return an error");
}

const VALIDATION_YAML: &str = r#"
objective: "test validation"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
validation:
  commands:
    - cargo fmt --check
    - cargo test
  timeout_seconds: 120
"#;

#[test]
fn parses_validation_config() {
    let tmp = TempYaml::new(VALIDATION_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let v = config.validation.expect("validation must be present");
    assert_eq!(v.commands, vec!["cargo fmt --check", "cargo test"]);
    assert_eq!(v.timeout_seconds, Some(120));
}

#[test]
fn validation_absent_defaults_to_none() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert!(
        config.validation.is_none(),
        "missing validation section must deserialize as None"
    );
}

#[test]
fn invalid_yaml_returns_error() {
    let tmp = TempYaml::new("not: valid: yaml: [");
    let result = ForgeConfig::from_file(tmp.path());
    assert!(result.is_err(), "invalid YAML must return an error");
}

const STRONG_PROVIDER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
  strong:
    unmanaged:
      base_url: "http://localhost:8081"
      model: "llama-strong-test"
      n_predict: 1024
  strong_timeout_seconds: 180
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn config_parses_optional_strong_provider_fields() {
    let tmp = TempYaml::new(STRONG_PROVIDER_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let ProviderTierConfig::Unmanaged(strong) = config
        .provider
        .strong
        .as_ref()
        .expect("strong tier must parse")
    else {
        panic!("strong provider must parse as unmanaged");
    };
    assert_eq!(strong.base_url, "http://localhost:8081");
    assert_eq!(strong.model, "llama-strong-test");
    assert_eq!(strong.n_predict, 1024);
    assert_eq!(config.provider.strong_timeout_seconds, Some(180));
}

#[test]
fn strong_provider_fields_absent_defaults_to_none() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert!(config.provider.strong.is_none());
    assert!(config.provider.strong_timeout_seconds.is_none());
}

const MISSING_PROVIDER_MODEL_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn provider_model_is_required() {
    let tmp = TempYaml::new(MISSING_PROVIDER_MODEL_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "provider.cheap.unmanaged.model is required so run metadata can identify the expected model"
    );
}

const EMPTY_PROVIDER_MODEL_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "  "
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn provider_model_must_not_be_blank() {
    let tmp = TempYaml::new(EMPTY_PROVIDER_MODEL_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "blank (whitespace-only) provider.cheap.unmanaged.model must be rejected"
    );
}

const MANAGED_LLAMA_CPP_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    managed:
      llama_cpp:
        command: "llama-server"
        model:
          path: "models/coder.gguf"
        host: "127.0.0.1"
        port: 8080
        context_size: 8192
        startup_timeout_seconds: 45
        n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn parses_managed_llama_cpp_provider_config() {
    let tmp = TempYaml::new(MANAGED_LLAMA_CPP_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let ProviderTierConfig::Managed(managed) = config.provider.cheap else {
        panic!("managed provider config must parse");
    };
    assert_eq!(managed.llama_cpp.command, "llama-server");
    assert_eq!(
        managed.llama_cpp.model,
        ManagedLlamaCppModelConfig::Path("models/coder.gguf".to_string())
    );
    assert_eq!(managed.llama_cpp.host, "127.0.0.1");
    assert_eq!(managed.llama_cpp.port, 8080);
    assert_eq!(managed.llama_cpp.context_size, Some(8192));
    assert_eq!(managed.llama_cpp.startup_timeout_seconds, 45);
    assert_eq!(managed.llama_cpp.n_predict, 512);
}

const MANAGED_LLAMA_CPP_HF_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    managed:
      llama_cpp:
        command: "llama-server"
        model:
          hf: "lm-kit/qwen-3-8b-instruct-gguf:Q4_K_M"
        host: "127.0.0.1"
        port: 8080
        n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn parses_managed_llama_cpp_hf_model_config() {
    let tmp = TempYaml::new(MANAGED_LLAMA_CPP_HF_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let ProviderTierConfig::Managed(managed) = config.provider.cheap else {
        panic!("managed provider config must parse");
    };
    assert_eq!(
        managed.llama_cpp.model,
        ManagedLlamaCppModelConfig::HuggingFace(
            "lm-kit/qwen-3-8b-instruct-gguf:Q4_K_M".to_string()
        )
    );
}

const MIXED_PROVIDER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "models/coder.gguf"
      n_predict: 512
    managed:
      llama_cpp:
        command: "/opt/llama.cpp/llama-server"
        model:
          path: "models/coder.gguf"
        host: "127.0.0.1"
        port: 8080
        n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn provider_tier_rejects_mixed_managed_and_unmanaged_fields() {
    let tmp = TempYaml::new(MIXED_PROVIDER_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(result.is_err(), "mixed provider variants must not parse");
}

const MANAGED_LLAMA_CPP_MISSING_HOST_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    managed:
      llama_cpp:
        command: "llama-server"
        model:
          path: "models/coder.gguf"
        port: 8080
        n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn managed_llama_cpp_requires_host() {
    let tmp = TempYaml::new(MANAGED_LLAMA_CPP_MISSING_HOST_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "missing managed llama.cpp host must be rejected"
    );
}

const MANAGED_LLAMA_CPP_BLANK_COMMAND_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    managed:
      llama_cpp:
        command: " "
        model:
          path: "models/coder.gguf"
        host: "127.0.0.1"
        port: 8080
        n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn managed_llama_cpp_requires_non_blank_command() {
    let tmp = TempYaml::new(MANAGED_LLAMA_CPP_BLANK_COMMAND_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "blank (whitespace-only) managed llama.cpp command must be rejected"
    );
}

const MANAGED_LLAMA_CPP_ZERO_PORT_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    managed:
      llama_cpp:
        command: "llama-server"
        model:
          path: "models/coder.gguf"
        host: "127.0.0.1"
        port: 0
        n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn managed_llama_cpp_rejects_zero_port() {
    let tmp = TempYaml::new(MANAGED_LLAMA_CPP_ZERO_PORT_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "port 0 for managed llama.cpp must be rejected"
    );
}

const UNMANAGED_BASE_URL_MISSING_SCHEME_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn unmanaged_base_url_requires_scheme() {
    let tmp = TempYaml::new(UNMANAGED_BASE_URL_MISSING_SCHEME_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "base_url without a scheme must be rejected"
    );
}

const UNMANAGED_BASE_URL_BAD_SCHEME_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "ftp://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn unmanaged_base_url_rejects_unrecognized_scheme() {
    let tmp = TempYaml::new(UNMANAGED_BASE_URL_BAD_SCHEME_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "base_url with a non-http(s) scheme (ftp) must be rejected"
    );
}

const UNMANAGED_BASE_URL_MISSING_HOST_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: coding.yaml
"#;

#[test]
fn unmanaged_base_url_requires_host() {
    let tmp = TempYaml::new(UNMANAGED_BASE_URL_MISSING_HOST_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "base_url without a host (\"http://\") must be rejected"
    );
}

const ABSOLUTE_YAML: &str = r#"
objective: "test absolute paths"
artifact:
  repo_path: "/absolute/path/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "/absolute/telemetry"
adapter: coding.yaml
"#;

#[test]
fn absolute_paths_remain_absolute() {
    let tmp = TempYaml::new(ABSOLUTE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.artifact.repo_path, "/absolute/path/main.git",
        "absolute artifact path must not be altered"
    );
    assert_eq!(
        config.telemetry.directory, "/absolute/telemetry",
        "absolute telemetry directory must not be altered"
    );
}

// ── adapter config tests ─────────────────────────────────────────────────

const NO_ADAPTER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
"#;

#[test]
fn adapter_is_required_when_absent() {
    let tmp = TempYaml::new(NO_ADAPTER_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(result.is_err(), "absent adapter must be a hard error");
}

const BLANK_ADAPTER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: "   "
"#;

#[test]
fn adapter_is_required_when_blank() {
    let tmp = TempYaml::new(BLANK_ADAPTER_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "blank (whitespace-only) adapter must be a hard error"
    );
}

const PLANNER_ADAPTER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: planner.yaml
"#;

#[test]
fn config_parses_adapter() {
    let tmp = TempYaml::new(PLANNER_ADAPTER_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let config_dir = std::path::Path::new(tmp.path()).parent().unwrap();
    let expected = config_dir
        .join("planner.yaml")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        config.adapter, expected,
        "a relative adapter path must resolve against the config file's directory"
    );
}

#[test]
fn config_adapter_nested_relative_path_resolves_against_config_dir() {
    // Invariant: `adapter` is a full (possibly nested) path relative to the
    // config file, not a bare filename resolved against some separate
    // adapters directory.
    let config_dir = std::env::temp_dir().join(format!(
        "forge-rs-config-test-nested-adapter-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(config_dir.join("nested")).unwrap();
    // coding.yaml declares its plugins as `../plugins/...`, relative to its
    // own (nested) directory — so the plugins must be staged as siblings of
    // `nested/`, not flattened alongside it like `stage_fixture` normally
    // does for the top-level fixture layout.
    let nested_coding_yaml = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("coding.yaml"),
    )
    .unwrap();
    std::fs::write(
        config_dir.join("nested").join("coding.yaml"),
        nested_coding_yaml,
    )
    .unwrap();
    std::fs::create_dir_all(config_dir.join("plugins")).unwrap();
    for name in ["python.yaml", "rust.yaml"] {
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins")
                .join(name),
            config_dir.join("plugins").join(name),
        )
        .unwrap();
    }

    let yaml = EXAMPLE_YAML.replace("adapter: coding.yaml", "adapter: nested/coding.yaml");
    let config_path = config_dir.join("forge.yaml");
    std::fs::write(&config_path, &yaml).unwrap();

    let config = ForgeConfig::from_file(config_path.to_str().unwrap()).unwrap();
    let expected = config_dir
        .join("nested/coding.yaml")
        .to_string_lossy()
        .into_owned();
    assert_eq!(config.adapter, expected);

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn config_absolute_adapter_path_remains_absolute() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("coding.yaml");
    let yaml = EXAMPLE_YAML.replace(
        "adapter: coding.yaml",
        &format!("adapter: \"{}\"", path.display()),
    );
    let tmp = TempYaml::new(&yaml);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(config.adapter, path.to_string_lossy());
}

const UNKNOWN_ADAPTER_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  cheap:
    unmanaged:
      base_url: "http://localhost:8080"
      model: "llama-test"
      n_predict: 512
telemetry:
  directory: "runs"
adapter: bogus_adapter_that_does_not_exist.yaml
"#;

#[test]
fn unknown_adapter_filename_fails_at_config_load_time() {
    // Invariant: an adapter path that does not exist on disk must fail
    // from_file itself, not wait until the run actually starts, with a
    // clear error naming the adapter path.
    let tmp = TempYaml::new(UNKNOWN_ADAPTER_YAML);
    let err = ForgeConfig::from_file(tmp.path()).unwrap_err();
    assert!(
        err.to_string()
            .contains("bogus_adapter_that_does_not_exist.yaml"),
        "error must name the missing adapter path; got: {err}"
    );
}

// ── plugin config tests ──────────────────────────────────────────────────
//
// Language plugins are now declared in the adapter YAML's `plugins:` list,
// not in `ForgeConfig` — see `src/project/loader_tests.rs` for plugin
// loading/resolution coverage. `ForgeConfig::from_file` still fails loudly
// when the adapter (or any plugin it declares) fails to load, exercised by
// `unknown_adapter_fails_loudly` below.
