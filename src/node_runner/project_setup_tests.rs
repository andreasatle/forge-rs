use super::*;
use crate::config::ValidationConfig;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn adapter_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters")
        .join(name)
}

/// Path to a test-fixture adapter YAML (not a built-in, user-facing one).
fn fixture_adapter_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name)
}

fn plugin_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(name)
}

fn builder<'a>(
    adapter: &str,
    validation: Option<&'a ValidationConfig>,
) -> ProjectRuntimeSetupBuilder<'a> {
    ProjectRuntimeSetupBuilder::new(&adapter_path(adapter), validation, "").unwrap()
}

/// `language` matching one of `coding.yaml`'s declared plugin extensions
/// (`"py"`/`"rs"`) — most callers don't care which, since only
/// `active_plugin()`'s consumers (`api_summary_command`,
/// `primary_language_init`, the handler-level fallback `validator()`) are
/// affected by it; per-node lookups (`select_plugin`,
/// `validation_plan_for_role_fn`) key off target-file extension instead.
fn fixture_builder<'a>(
    adapter: &str,
    language: &str,
    validation: Option<&'a ValidationConfig>,
) -> ProjectRuntimeSetupBuilder<'a> {
    ProjectRuntimeSetupBuilder::new(&fixture_adapter_path(adapter), validation, language).unwrap()
}

/// A fresh, never-before-used temp directory for one test's fixture files.
fn test_dir(label: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "forge-rs-project-setup-test-{}-{id}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal, self-contained adapter config with two worker roles
/// (`implementer`, `tester`) and no `plugins:` field — callers append their
/// own `plugins:` block as needed.
const CUSTOM_ADAPTER_YAML: &str = r#"
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
  - key: implementer
    description: "Implements code."
    producer:
      identity: "impl identity"
      context: "impl context"
      instructions: "impl instructions"
      constraints: "impl constraints"
    critic:
      identity: "impl critic identity"
      context: "impl critic context"
      instructions: "impl critic instructions"
      constraints: "impl critic constraints"
    referee:
      identity: "impl referee identity"
      context: "impl referee context"
      instructions: "impl referee instructions"
      constraints: "impl referee constraints"
  - key: tester
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
"#;

/// Writes `CUSTOM_ADAPTER_YAML` plus a `plugins:` block naming `plugin_refs`
/// (as written, unresolved) to a fresh file in `dir`, returning its path.
fn write_custom_adapter(dir: &Path, plugin_refs: &[&str]) -> PathBuf {
    let mut yaml = CUSTOM_ADAPTER_YAML.to_string();
    if !plugin_refs.is_empty() {
        yaml.push_str("plugins:\n");
        for reference in plugin_refs {
            yaml.push_str(&format!("  - {reference}\n"));
        }
    }
    let path = dir.join("adapter.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

// ── adapter selection ────────────────────────────────────────────────────

#[test]
fn runtime_selects_coding_adapter() {
    let policy = fixture_builder("coding.yaml", "py", None).role_policy();
    assert!(
        policy.planner_producer_base.contains("software planning"),
        "coding adapter must produce software-planning planner prompt; got:\n{}",
        policy.planner_producer_base
    );
    assert!(
        policy
            .worker_producer_system
            .contains("software implementation"),
        "coding adapter must produce software-implementation worker prompt; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn runtime_selects_planner_adapter() {
    let policy = builder("planner.yaml", None).role_policy();
    assert!(
        policy
            .planner_producer_base
            .contains("further decomposition or a single, self-contained task"),
        "planner adapter must select the decomposition-or-task planner prompt; got:\n{}",
        policy.planner_producer_base
    );
}

#[test]
fn runtime_selects_create_test_adapter() {
    let policy = builder("create_test.yaml", None).role_policy();
    assert!(
        policy
            .worker_producer_system
            .contains("import the functions under test"),
        "create_test adapter must select the test-writing worker prompt; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn unknown_adapter_fails_loudly() {
    let result = ProjectRuntimeSetupBuilder::new(&adapter_path("bogus.yaml"), None, "");
    assert!(result.is_err(), "unrecognised adapter must be a hard error");
}

// ── language plugin loading ──────────────────────────────────────────────

#[test]
fn unknown_plugin_fails_loudly() {
    let dir = test_dir("unknown-plugin");
    let adapter = write_custom_adapter(&dir, &["does_not_exist.yaml"]);
    let result = ProjectRuntimeSetupBuilder::new(&adapter, None, "");
    assert!(result.is_err(), "unrecognised plugin must be a hard error");
}

#[test]
fn adapter_validation_function_missing_from_plugin_functions_fails_loudly() {
    // Invariant: every validation function name a worker role selects (see
    // `WorkerRoleConfig::validation`) must exist in every declared plugin's
    // `functions` map — a plugin that doesn't define a selected name is a
    // hard error at config load time, regardless of which plugin ends up
    // selected for a given node.
    let dir = test_dir("missing-function");
    std::fs::write(
        dir.join("plugin.yaml"),
        r#"
extensions: [zz]
init:
  commands: []
validation:
  commands: []
functions:
  lint:
    program: echo
    args: []
"#,
    )
    .unwrap();
    let mut yaml = CUSTOM_ADAPTER_YAML.replace(
        "  - key: implementer\n",
        "  - key: implementer\n    validation: [typecheck]\n",
    );
    yaml.push_str("plugins:\n  - plugin.yaml\n");
    let adapter = dir.join("adapter.yaml");
    std::fs::write(&adapter, yaml).unwrap();

    let err = match ProjectRuntimeSetupBuilder::new(&adapter, None, "") {
        Ok(_) => {
            panic!("adapter validation function missing from plugin functions must be a hard error")
        }
        Err(e) => e.to_string(),
    };
    assert_eq!(
        err,
        "worker role 'implementer' selects validation function 'typecheck', which is not defined in the plugin's functions for extension 'zz'"
    );
}

#[test]
fn worker_missing_key_is_fine_regardless_of_declared_plugins() {
    // Invariant: `key` is pure worker-role identity now — it has no
    // plugin-side counterpart to match against (that's `validation`'s job),
    // so a worker entry may omit it whether or not the adapter declares any
    // plugins.
    let dir = test_dir("missing-key-no-plugins");
    let yaml = CUSTOM_ADAPTER_YAML.replace("  - key: implementer\n", "  - \n");
    let adapter = dir.join("adapter.yaml");
    std::fs::write(&adapter, yaml).unwrap();

    let result = ProjectRuntimeSetupBuilder::new(&adapter, None, "");
    assert!(
        result.is_ok(),
        "worker entry missing key must load fine with no plugins declared"
    );

    let dir = test_dir("missing-key-with-plugins");
    std::fs::write(
        dir.join("plugin.yaml"),
        r#"
extensions: [zz]
init:
  commands: []
validation:
  commands: []
functions: {}
"#,
    )
    .unwrap();
    let mut yaml = CUSTOM_ADAPTER_YAML.replace("  - key: implementer\n", "  - \n");
    yaml.push_str("plugins:\n  - plugin.yaml\n");
    let adapter = dir.join("adapter.yaml");
    std::fs::write(&adapter, yaml).unwrap();

    let is_ok = ProjectRuntimeSetupBuilder::new(&adapter, None, "").is_ok();
    assert!(
        is_ok,
        "worker entry missing key must also load fine with plugins declared, since \
         key no longer selects anything plugin-side"
    );
}

// ── required_test_targets_fn ─────────────────────────────────────────────

#[test]
fn required_test_targets_fn_selects_plugin_by_target_extension() {
    // Invariant: the adapter's declared plugins are keyed by extension, so a
    // node's target files pick the matching plugin's derivation rules — a
    // Python target gets the Python rule, a Rust target gets the Rust rule.
    let setup = fixture_builder("coding.yaml", "py", None);
    let f = setup.required_test_targets_fn();
    assert_eq!(
        f(&["main.py".to_string()]),
        vec!["tests/test_main.py".to_string()]
    );
    assert_eq!(f(&["lib.rs".to_string()]), vec!["lib_test.rs".to_string()]);
}

#[test]
fn required_test_targets_fn_empty_when_no_plugin_matches_extension() {
    let setup = fixture_builder("coding.yaml", "py", None);
    let f = setup.required_test_targets_fn();
    assert!(
        f(&["README.md".to_string()]).is_empty(),
        "a target extension with no matching plugin must derive no test targets"
    );
}

// ── validator ─────────────────────────────────────────────────────────────

#[test]
fn runtime_uses_always_pass_when_validation_and_plugins_absent() {
    use crate::artifacts::Workspace;

    let dir = test_dir("always-pass");
    let adapter = write_custom_adapter(&dir, &[]);
    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    let validator = ProjectRuntimeSetupBuilder::new(&adapter, None, "")
        .unwrap()
        .validator();
    let result = validator.validate(&ws);
    assert!(
        result.passed,
        "no validation config and no plugins must yield a passing validator"
    );
}

#[test]
fn runtime_uses_command_validator_when_configured() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    // A failing command proves the CommandValidator is active, not AlwaysPassValidator.
    let config = ValidationConfig {
        commands: vec!["false".to_string()],
        timeout_seconds: None,
    };
    let validator = fixture_builder("coding.yaml", "py", Some(&config)).validator();
    let result = validator.validate(&ws);
    assert!(
        !result.passed,
        "configured command validator must run commands and fail on non-zero exit"
    );
}

#[test]
fn runtime_language_validator_uses_first_plugins_commands() {
    use crate::artifacts::Workspace;

    // The rust plugin is declared alone, so it is deterministically "first"
    // regardless of extension ordering; its cargo commands fail in a bare
    // temp dir, confirming a real CommandValidator (not AlwaysPassValidator)
    // was wired up as the handler-level fallback.
    let dir = test_dir("rust-validator");
    let rust_plugin_path = plugin_path("rust.yaml").display().to_string();
    let adapter = write_custom_adapter(&dir, &[rust_plugin_path.as_str()]);
    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    let validator = ProjectRuntimeSetupBuilder::new(&adapter, None, "rs")
        .unwrap()
        .validator();
    let result = validator.validate(&ws);
    assert!(
        !result.passed,
        "rust language validator must run cargo commands that fail in a temp dir; got: {}",
        result.summary
    );
}

#[test]
fn runtime_backward_compat_validation_yaml_translates_to_sh_wrapper() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    // Raw YAML commands are wrapped in sh -c for backward compatibility, and
    // take priority over any configured plugins.
    let config = ValidationConfig {
        commands: vec!["true".to_string()],
        timeout_seconds: None,
    };
    let validator = fixture_builder("coding.yaml", "py", Some(&config)).validator();
    let result = validator.validate(&ws);
    assert!(
        result.passed,
        "sh-wrapped 'true' must pass via backward-compat translation; got: {}",
        result.summary
    );
}

// ── ProjectRuntimeSetup::build ───────────────────────────────────────────

#[test]
fn build_wires_validation_plan_api_summary_and_primary_language_init() {
    let setup =
        ProjectRuntimeSetup::build(&fixture_adapter_path("coding.yaml"), None, "py").unwrap();
    assert!(
        (setup.validation_plan_for_role_fn)(None, &["main.rs".to_string()]).is_some(),
        "a target matching a configured plugin's extension must produce a validation plan"
    );
    assert!(
        setup.api_summary_command.is_some(),
        "build() must surface the first configured plugin's api_summary command"
    );
    assert!(
        setup.primary_language_init.is_some(),
        "build() must surface the first configured plugin's init spec for repo bootstrap"
    );
}

#[test]
fn primary_language_init_is_unaffected_by_worker_validation_selection() {
    // Invariant: `primary_language_init` (repo-bootstrap commands, e.g.
    // `uv init`/`cargo init`) is derived solely from the active plugin's own
    // `init:` field — never from any worker role's `validation:` selection.
    // A role with an empty/omitted `validation:` list must see the exact
    // same init spec as a role that selects every named function; no team
    // can suppress or alter it via function selection.
    let dir = test_dir("primary-language-init-selection-independence");
    std::fs::write(
        dir.join("plugin.yaml"),
        r#"
extensions: [zz]
init:
  commands:
    - program: echo
      args: ["bootstrap"]
validation:
  commands: []
functions:
  lint:
    program: echo
    args: ["lint"]
  typecheck:
    program: echo
    args: ["typecheck"]
"#,
    )
    .unwrap();

    // implementer: no `validation:` at all (empty selection).
    let mut empty_yaml = CUSTOM_ADAPTER_YAML.to_string();
    empty_yaml.push_str("plugins:\n  - plugin.yaml\n");
    let empty_adapter = dir.join("adapter_empty.yaml");
    std::fs::write(&empty_adapter, empty_yaml).unwrap();

    // implementer: selects every named function this plugin defines.
    let mut full_yaml = CUSTOM_ADAPTER_YAML.replace(
        "  - key: implementer\n",
        "  - key: implementer\n    validation: [lint, typecheck]\n",
    );
    full_yaml.push_str("plugins:\n  - plugin.yaml\n");
    let full_adapter = dir.join("adapter_full.yaml");
    std::fs::write(&full_adapter, full_yaml).unwrap();

    let empty_setup = ProjectRuntimeSetup::build(&empty_adapter, None, "zz").unwrap();
    let full_setup = ProjectRuntimeSetup::build(&full_adapter, None, "zz").unwrap();

    let empty_init = empty_setup
        .primary_language_init
        .expect("init spec must be present for a role with empty validation selection");
    let full_init = full_setup
        .primary_language_init
        .expect("init spec must be present for a role with full validation selection");

    assert_eq!(
        empty_init, full_init,
        "primary_language_init must be identical regardless of any worker role's validation selection"
    );
    assert_eq!(
        empty_init.commands[0].program, "echo",
        "init spec must still carry the plugin's real bootstrap command"
    );
}

#[test]
fn real_adapters_resolve_to_the_same_validation_plans_as_the_old_role_keyed_bundles() {
    // Invariant: this pins the exact command/scope/gating shape each real
    // adapter's role resolved to under the old role-keyed bundle
    // mechanism, now produced by resolving `validation: [...]` names against
    // the python plugin's `functions` map instead — a pure mechanism swap,
    // not a behavior change. See adapters/create_test.yaml, implement.yaml,
    // pass_tests.yaml and plugins/python.yaml's `functions` map.
    let targets = vec!["main.py".to_string()];

    let create_test =
        ProjectRuntimeSetup::build(&adapter_path("create_test.yaml"), None, "py").unwrap();
    let tester_plan = (create_test.validation_plan_for_role_fn)(Some("tester"), &targets)
        .expect("create_test's tester role must produce a validation plan");
    assert_eq!(
        tester_plan.steps.len(),
        1,
        "tester plan must have exactly one step; got: {:?}",
        tester_plan.steps
    );
    assert_eq!(
        tester_plan.steps[0].command,
        vec!["uv", "run", "ruff", "check"]
    );
    assert_eq!(tester_plan.steps[0].scope, ValidationScope::ChangedFiles);
    assert!(tester_plan.steps[0].when_artifacts_present.is_empty());

    let implement =
        ProjectRuntimeSetup::build(&adapter_path("implement.yaml"), None, "py").unwrap();
    let implementer_plan = (implement.validation_plan_for_role_fn)(Some("implementer"), &targets)
        .expect("implement's implementer role must produce a validation plan");
    assert_eq!(
        implementer_plan
            .steps
            .iter()
            .map(|s| s.command.clone())
            .collect::<Vec<_>>(),
        vec![
            vec!["uv", "run", "ruff", "check"],
            vec!["uv", "run", "pyright"],
        ],
        "implementer plan must be exactly ruff then pyright, no pytest; got: {:?}",
        implementer_plan.steps
    );
    assert_eq!(
        implementer_plan.steps[0].scope,
        ValidationScope::ChangedFiles
    );
    assert_eq!(implementer_plan.steps[1].scope, ValidationScope::Workspace);

    let pass_tests =
        ProjectRuntimeSetup::build(&adapter_path("pass_tests.yaml"), None, "py").unwrap();
    let pass_tests_plan = (pass_tests.validation_plan_for_role_fn)(Some("pass_tests"), &targets)
        .expect("pass_tests's pass_tests role must produce a validation plan");
    assert_eq!(
        pass_tests_plan
            .steps
            .iter()
            .map(|s| s.command.clone())
            .collect::<Vec<_>>(),
        vec![
            vec!["uv", "run", "ruff", "check"],
            vec!["uv", "run", "pyright"],
            vec!["uv", "run", "pytest"],
        ],
        "pass_tests plan must be exactly ruff, pyright, pytest, in order; got: {:?}",
        pass_tests_plan.steps
    );
    assert_eq!(
        pass_tests_plan.steps[0].scope,
        ValidationScope::ChangedFiles
    );
    assert_eq!(pass_tests_plan.steps[1].scope, ValidationScope::Workspace);
    assert_eq!(pass_tests_plan.steps[2].scope, ValidationScope::Workspace);
    assert!(
        pass_tests_plan.steps[2].when_artifacts_present.is_empty(),
        "pass_tests's pytest step must remain ungated (the pytest-gate fix this session preserves)"
    );
}

#[test]
fn validation_plan_for_role_uses_adapters_tester_validation_selection() {
    // Invariant: a `tester`-role Work node targeting a Python file gets a
    // validation plan built from exactly the named functions the adapter's
    // `tester` worker selects (`validation: [lint]`, resolved against the
    // python plugin's `functions` map) rather than the plugin's default plan
    // (ruff + pyright + pytest), so tester nodes aren't required to pass the
    // full test suite before their own test files exist.
    let setup =
        ProjectRuntimeSetup::build(&fixture_adapter_path("coding.yaml"), None, "py").unwrap();
    let plan = (setup.validation_plan_for_role_fn)(Some("tester"), &["main.py".to_string()])
        .expect("adapter's tester validation selection must produce a validation plan");
    assert_eq!(
        plan.steps.len(),
        1,
        "tester validation plan must contain exactly one step; got: {:?}",
        plan.steps
    );
    assert!(
        plan.steps[0].command.contains(&"ruff".to_string()),
        "tester validation plan must run ruff (the 'lint' function); got: {:?}",
        plan.steps[0].command
    );
    assert!(
        !plan.steps[0].command.contains(&"pytest".to_string()),
        "tester validation plan must not run pytest"
    );
}

#[test]
fn validation_plan_for_role_runs_pytest_unconditionally_for_pass_tests_role() {
    // Invariant: a `pass_tests`-role Work node targeting a source-only file
    // (not itself a test file) still gets a pytest step in its validation
    // plan, with no `when_artifacts_present` gate. `pass_tests` is dispatched
    // only after both `implement` and `create_test` have already integrated
    // (`after_teams(implement, create_test)` in forge.yaml), so by the time
    // this plan runs, the real test file is guaranteed to exist — gating
    // pytest on the node's own `target_files` (as the shared `implementer`
    // role used to) meant pytest was skipped for every source-only node,
    // regardless of whether tests actually existed in the workspace.
    let setup = ProjectRuntimeSetup::build(&adapter_path("pass_tests.yaml"), None, "py").unwrap();
    let plan = (setup.validation_plan_for_role_fn)(Some("pass_tests"), &["main.py".to_string()])
        .expect("python plugin's pass_tests role override must produce a validation plan");
    let pytest_step = plan
        .steps
        .iter()
        .find(|s| s.command.contains(&"pytest".to_string()))
        .expect("pass_tests validation plan must include a pytest step");
    assert!(
        pytest_step.when_artifacts_present.is_empty(),
        "pass_tests's pytest step must run unconditionally, not gated on target_files; got: {:?}",
        pytest_step.when_artifacts_present
    );
    assert_eq!(
        pytest_step.scope,
        ValidationScope::Workspace,
        "pass_tests's pytest step must cover the whole workspace, not just the node's own target_files"
    );
}

#[test]
fn validation_plan_for_role_implementer_no_longer_runs_pytest() {
    // Invariant: verifying that tests pass is `pass_tests`'s job, not
    // `implement`'s — `implement`'s plan checks its own code (ruff, pyright)
    // but must not include a pytest step at all.
    let setup = ProjectRuntimeSetup::build(&adapter_path("implement.yaml"), None, "py").unwrap();
    let plan = (setup.validation_plan_for_role_fn)(Some("implementer"), &["main.py".to_string()])
        .expect("python plugin's implementer role override must produce a validation plan");
    assert!(
        !plan
            .steps
            .iter()
            .any(|s| s.command.contains(&"pytest".to_string())),
        "implementer validation plan must not run pytest; got: {:?}",
        plan.steps
    );
}

#[test]
fn made_up_function_name_resolves_with_zero_rust_changes() {
    // Invariant / genericity proof: Rust never matches on any specific
    // validation function name — a plugin exposing a function under an
    // arbitrary, never-before-seen name, selected by an adapter's worker
    // role under that same made-up name, resolves correctly with no
    // additional Rust code beyond what this session already built. If this
    // test needed a Rust change to pass, the mechanism would not actually be
    // generic.
    let dir = test_dir("genericity-proof");
    std::fs::write(
        dir.join("plugin.yaml"),
        r#"
extensions: [zz]
init:
  commands: []
validation:
  commands: []
functions:
  frobnicate:
    program: echo
    args: ["frobnicated"]
    scope: workspace
"#,
    )
    .unwrap();
    let mut yaml = CUSTOM_ADAPTER_YAML.replace(
        "  - key: implementer\n",
        "  - key: implementer\n    validation: [frobnicate]\n",
    );
    yaml.push_str("plugins:\n  - plugin.yaml\n");
    let adapter = dir.join("adapter.yaml");
    std::fs::write(&adapter, yaml).unwrap();

    let setup = ProjectRuntimeSetupBuilder::new(&adapter, None, "zz")
        .unwrap()
        .build();
    let plan = (setup.validation_plan_for_role_fn)(Some("implementer"), &["main.zz".to_string()])
        .expect("a made-up function name selected by the adapter must produce a validation plan");
    assert_eq!(
        plan.steps.len(),
        1,
        "plan must contain exactly the one selected function's step; got: {:?}",
        plan.steps
    );
    assert_eq!(plan.steps[0].command, vec!["echo", "frobnicated"]);
    assert_eq!(plan.steps[0].scope, ValidationScope::Workspace);
}

#[test]
fn validation_plan_for_role_falls_back_to_default_when_role_has_no_override() {
    // Invariant: a role absent from the selected plugin's `roles` list (e.g.
    // any role that isn't one of the adapter's configured worker roles) must
    // not silently drop validation — it falls back to the same default plan
    // used for nodes with no role assigned, for the same target files.
    let setup =
        ProjectRuntimeSetup::build(&fixture_adapter_path("coding.yaml"), None, "py").unwrap();
    let targets = vec!["main.rs".to_string()];
    assert_eq!(
        (setup.validation_plan_for_role_fn)(Some("reviewer"), &targets),
        (setup.validation_plan_for_role_fn)(None, &targets),
        "a role with no override must fall back to the default validation plan"
    );
}

#[test]
fn validation_plan_for_role_falls_back_to_explicit_validation_when_no_plugin_matches() {
    // Invariant: when a node's target files match no configured plugin's
    // extension, the explicit `validation:` config (when present) still
    // applies, rather than silently producing no plan.
    let config = ValidationConfig {
        commands: vec!["true".to_string()],
        timeout_seconds: None,
    };
    let setup =
        ProjectRuntimeSetup::build(&fixture_adapter_path("coding.yaml"), Some(&config), "py")
            .unwrap();
    let plan = (setup.validation_plan_for_role_fn)(None, &["README.md".to_string()])
        .expect("no plugin match must fall back to the explicit validation config");
    assert_eq!(
        plan.steps[0].command,
        vec!["sh".to_string(), "-c".to_string(), "true".to_string()]
    );
}

#[test]
fn validation_plan_for_role_is_none_when_no_plugin_matches_and_no_explicit_validation() {
    let setup =
        ProjectRuntimeSetup::build(&fixture_adapter_path("coding.yaml"), None, "py").unwrap();
    assert!(
        (setup.validation_plan_for_role_fn)(None, &["README.md".to_string()]).is_none(),
        "no matching plugin and no explicit validation must produce no plan"
    );
}
