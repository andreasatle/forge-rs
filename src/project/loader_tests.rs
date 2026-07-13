use super::*;
use crate::project::ProjectAdapter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh, never-before-used file path for one test.
fn unique_path(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "forge-rs-adapter-test-{}-{id}-{name}",
        std::process::id()
    ))
}

/// Path to a built-in adapter YAML shipped alongside the crate.
fn repo_adapter(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters")
        .join(name)
}

const CUSTOM_ADAPTER_YAML: &str = r#"
planner:
  producer:
    identity: "custom planner identity"
    context: "custom planner context"
    instructions: "custom planner instructions"
    constraints: "custom planner constraints"
  critic:
    identity: "custom critic identity"
    context: "custom critic context"
    instructions: "custom critic instructions"
    constraints: "custom critic constraints"
  referee:
    identity: "custom referee identity"
    context: "custom referee context"
    instructions: "custom referee instructions"
    constraints: "custom referee constraints"
workers:
  - plugin_role: implementer
    description: "Implements code changes."
    producer:
      identity: "custom worker identity"
      context: "custom worker context"
      instructions: "custom worker instructions"
      constraints: "custom worker constraints"
    critic:
      identity: "custom worker critic identity"
      context: "custom worker critic context"
      instructions: "custom worker critic instructions"
      constraints: "custom worker critic constraints"
    referee:
      identity: "custom worker referee identity"
      context: "custom worker referee context"
      instructions: "custom worker referee instructions"
      constraints: "custom worker referee constraints"
"#;

// ── built-in adapters ────────────────────────────────────────────────────

#[test]
fn coding_adapter_loads_from_its_shipped_path() {
    let adapter = load_adapter(&repo_adapter("coding.yaml")).unwrap();
    assert!(!adapter.role_policy().planner_producer_base.is_empty());
}

// ── user-defined adapters ────────────────────────────────────────────────

#[test]
fn user_defined_adapter_loads_from_any_path_with_no_rust_changes() {
    let path = unique_path("my_project.yaml");
    fs::write(&path, CUSTOM_ADAPTER_YAML).unwrap();

    let adapter = load_adapter(&path).unwrap();
    assert!(
        adapter
            .role_policy()
            .worker_producer_system
            .contains("custom worker instructions")
    );
}

#[test]
fn missing_adapter_file_is_a_hard_error() {
    let path = unique_path("bogus.yaml");
    let err = load_adapter(&path).unwrap_err();
    assert!(
        err.to_string().contains("failed to read adapter"),
        "missing adapter file must fail with a clear read error; got: {err}"
    );
}

#[test]
fn invalid_adapter_content_is_a_hard_error() {
    let path = unique_path("invalid.yaml");
    fs::write(&path, "not: [valid, adapter").unwrap();
    let err = load_adapter(&path).unwrap_err();
    assert!(
        err.to_string().contains("not a valid adapter config"),
        "invalid adapter YAML must fail with a parse error; got: {err}"
    );
}

// ── language plugins ─────────────────────────────────────────────────────

const CUSTOM_PLUGIN_YAML: &str = r#"
extensions: [ping]
identity: "custom plugin guidance"
init:
  commands: []
validation:
  commands: []
"#;

#[test]
fn adapter_without_plugins_field_has_no_language_plugins() {
    let path = unique_path("no_plugins.yaml");
    fs::write(&path, CUSTOM_ADAPTER_YAML).unwrap();

    let adapter = load_adapter(&path).unwrap();
    assert!(
        adapter.language_plugins().is_empty(),
        "an adapter with no plugins: field must have no language plugins"
    );
}

#[test]
fn adapter_loads_declared_plugins_keyed_by_extension() {
    let plugin_path = unique_path("plugin.yaml");
    fs::write(&plugin_path, CUSTOM_PLUGIN_YAML).unwrap();

    let adapter_path = unique_path("with_plugin.yaml");
    let yaml = format!(
        "{CUSTOM_ADAPTER_YAML}plugins:\n  - {}\n",
        plugin_path.file_name().unwrap().to_string_lossy()
    );
    fs::write(&adapter_path, yaml).unwrap();

    let adapter = load_adapter(&adapter_path).unwrap();
    assert_eq!(
        adapter
            .language_plugins()
            .get("ping")
            .map(|spec| spec.identity.as_str()),
        Some("custom plugin guidance"),
        "plugin path must be resolved relative to the adapter file's own directory"
    );
}

#[test]
fn adapter_role_policy_never_carries_plugin_prompt_content() {
    // Invariant: load_adapter composes only the generic and adapter prompt
    // layers into role_policy(), regardless of declared plugins — plugin
    // guidance is selected and injected per node from that node's own
    // target files (see the node_runner deliberation context tests), not
    // baked into the adapter-wide policy where every node would see every
    // declared plugin's content.
    let plugin_path = unique_path("plugin.yaml");
    fs::write(&plugin_path, CUSTOM_PLUGIN_YAML).unwrap();

    let adapter_path = unique_path("with_plugin.yaml");
    let yaml = format!(
        "{CUSTOM_ADAPTER_YAML}plugins:\n  - {}\n",
        plugin_path.file_name().unwrap().to_string_lossy()
    );
    fs::write(&adapter_path, yaml).unwrap();

    let policy = load_adapter(&adapter_path).unwrap().role_policy();
    assert!(
        !policy
            .planner_producer_base
            .contains("custom plugin guidance"),
        "role_policy() must not carry any plugin's prompt content; got:\n{}",
        policy.planner_producer_base
    );
}

#[test]
fn adapter_with_unknown_plugin_path_fails_loudly() {
    let adapter_path = unique_path("bogus_plugin.yaml");
    let yaml = format!("{CUSTOM_ADAPTER_YAML}plugins:\n  - does_not_exist.yaml\n");
    fs::write(&adapter_path, yaml).unwrap();

    let err = load_adapter(&adapter_path).unwrap_err();
    assert!(
        err.to_string().contains("does_not_exist.yaml"),
        "error must name the missing plugin path; got: {err}"
    );
}

// ── bundled single-purpose adapter content ───────────────────────────────
//
// These protect each bundled team adapter's intent: the create_test worker
// importing from the module under test rather than reimplementing it, and
// every bundled adapter loading with its README.md context file intact.

#[test]
fn create_test_worker_producer_prompt_requires_importing_functions_under_test() {
    let policy = load_adapter(&repo_adapter("create_test.yaml"))
        .unwrap()
        .role_policy();
    assert!(
        policy
            .worker_producer_system
            .contains("import the functions under test"),
        "create_test worker prompt must require importing functions under test; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn bundled_adapters_context_file_names_include_readme() {
    for name in [
        "coding.yaml",
        "planner.yaml",
        "implement.yaml",
        "create_test.yaml",
        "pass_tests.yaml",
    ] {
        let adapter = load_adapter(&repo_adapter(name)).unwrap();
        assert!(
            adapter
                .context_file_names()
                .contains(&"README.md".to_string()),
            "{name} must include README.md in context_files"
        );
    }
}
