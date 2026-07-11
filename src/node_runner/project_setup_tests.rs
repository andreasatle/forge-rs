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

fn plugin_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(name)
}

fn builder<'a>(
    adapter: &str,
    validation: Option<&'a ValidationConfig>,
) -> ProjectRuntimeSetupBuilder<'a> {
    ProjectRuntimeSetupBuilder::new(&adapter_path(adapter), validation).unwrap()
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
  - role: implementer
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
  - role: tester
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
    let policy = builder("coding.yaml", None).role_policy();
    assert!(
        policy.planner_producer_system.contains("software planning"),
        "coding adapter must produce software-planning planner prompt; got:\n{}",
        policy.planner_producer_system
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
            .planner_producer_system
            .contains("further decomposition or a single, self-contained task"),
        "planner adapter must select the decomposition-or-task planner prompt; got:\n{}",
        policy.planner_producer_system
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
    let result = ProjectRuntimeSetupBuilder::new(&adapter_path("bogus.yaml"), None);
    assert!(result.is_err(), "unrecognised adapter must be a hard error");
}

// ── language plugin loading ──────────────────────────────────────────────

#[test]
fn unknown_plugin_fails_loudly() {
    let dir = test_dir("unknown-plugin");
    let adapter = write_custom_adapter(&dir, &["does_not_exist.yaml"]);
    let result = ProjectRuntimeSetupBuilder::new(&adapter, None);
    assert!(result.is_err(), "unrecognised plugin must be a hard error");
}

#[test]
fn adapter_role_missing_from_plugin_roles_fails_loudly() {
    // Invariant: every worker role the adapter defines must have a matching
    // entry in every declared plugin's `roles` list — a plugin that only
    // covers some of the adapter's roles is a hard error at config load
    // time, regardless of which plugin ends up selected for a given node.
    let dir = test_dir("missing-role");
    std::fs::write(
        dir.join("plugin.yaml"),
        r#"
extensions: [zz]
init:
  commands: []
validation:
  commands: []
roles:
  - role: implementer
    validation:
      commands: []
"#,
    )
    .unwrap();
    let adapter = write_custom_adapter(&dir, &["plugin.yaml"]);

    let err = match ProjectRuntimeSetupBuilder::new(&adapter, None) {
        Ok(_) => panic!("adapter role missing from plugin roles must be a hard error"),
        Err(e) => e.to_string(),
    };
    assert_eq!(
        err,
        "adapter role 'tester' is not defined in the plugin for extension 'zz'"
    );
}

// ── required_test_targets_fn ─────────────────────────────────────────────

#[test]
fn required_test_targets_fn_selects_plugin_by_target_extension() {
    // Invariant: the adapter's declared plugins are keyed by extension, so a
    // node's target files pick the matching plugin's derivation rules — a
    // Python target gets the Python rule, a Rust target gets the Rust rule.
    let setup = builder("coding.yaml", None);
    let f = setup.required_test_targets_fn();
    assert_eq!(
        f(&["main.py".to_string()]),
        vec!["tests/test_main.py".to_string()]
    );
    assert_eq!(f(&["lib.rs".to_string()]), vec!["lib_test.rs".to_string()]);
}

#[test]
fn required_test_targets_fn_empty_when_no_plugin_matches_extension() {
    let setup = builder("coding.yaml", None);
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
    let validator = ProjectRuntimeSetupBuilder::new(&adapter, None)
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
    let validator = builder("coding.yaml", Some(&config)).validator();
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
    let validator = ProjectRuntimeSetupBuilder::new(&adapter, None)
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
    let validator = builder("coding.yaml", Some(&config)).validator();
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
    let setup = ProjectRuntimeSetup::build(&adapter_path("coding.yaml"), None).unwrap();
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
fn validation_plan_for_role_uses_python_tester_role_override() {
    // Invariant: a `tester`-role Work node targeting a Python file gets the
    // python plugin's role-specific validation plan (ruff check only) rather
    // than the plugin's default plan (ruff + pyright + pytest), so tester
    // nodes aren't required to pass the full test suite before their own
    // test files exist.
    let setup = ProjectRuntimeSetup::build(&adapter_path("coding.yaml"), None).unwrap();
    let plan = (setup.validation_plan_for_role_fn)(Some("tester"), &["main.py".to_string()])
        .expect("python plugin's tester role override must produce a validation plan");
    assert_eq!(
        plan.steps.len(),
        1,
        "python tester validation plan must contain exactly one step; got: {:?}",
        plan.steps
    );
    assert!(
        plan.steps[0].command.contains(&"ruff".to_string()),
        "python tester validation plan must run ruff; got: {:?}",
        plan.steps[0].command
    );
    assert!(
        !plan.steps[0].command.contains(&"pytest".to_string()),
        "python tester validation plan must not run pytest"
    );
}

#[test]
fn validation_plan_for_role_falls_back_to_default_when_role_has_no_override() {
    // Invariant: a role absent from the selected plugin's `roles` list (e.g.
    // any role that isn't one of the adapter's configured worker roles) must
    // not silently drop validation — it falls back to the same default plan
    // used for nodes with no role assigned, for the same target files.
    let setup = ProjectRuntimeSetup::build(&adapter_path("coding.yaml"), None).unwrap();
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
    let setup = ProjectRuntimeSetup::build(&adapter_path("coding.yaml"), Some(&config)).unwrap();
    let plan = (setup.validation_plan_for_role_fn)(None, &["README.md".to_string()])
        .expect("no plugin match must fall back to the explicit validation config");
    assert_eq!(
        plan.steps[0].command,
        vec!["sh".to_string(), "-c".to_string(), "true".to_string()]
    );
}

#[test]
fn validation_plan_for_role_is_none_when_no_plugin_matches_and_no_explicit_validation() {
    let setup = ProjectRuntimeSetup::build(&adapter_path("coding.yaml"), None).unwrap();
    assert!(
        (setup.validation_plan_for_role_fn)(None, &["README.md".to_string()]).is_none(),
        "no matching plugin and no explicit validation must produce no plan"
    );
}
