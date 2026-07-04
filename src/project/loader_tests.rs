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
    instructions: "custom planner instructions"
    constraints: "custom planner constraints"
  critic:
    instructions: "custom critic instructions"
    constraints: "custom critic constraints"
  referee:
    instructions: "custom referee instructions"
    constraints: "custom referee constraints"
workers:
  - role: implementer
    description: "Implements code changes."
    producer:
      instructions: "custom worker instructions"
      constraints: "custom worker constraints"
    critic:
      instructions: "custom worker critic instructions"
      constraints: "custom worker critic constraints"
    referee:
      instructions: "custom worker referee instructions"
      constraints: "custom worker referee constraints"
"#;

// ── built-in adapters ────────────────────────────────────────────────────

#[test]
fn coding_adapter_loads_from_its_shipped_path() {
    let adapter = load_adapter(&repo_adapter("coding.yaml")).unwrap();
    assert!(!adapter.role_policy().planner_producer_system.is_empty());
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

// ── coding_tdd adapter content ───────────────────────────────────────────
//
// These protect the bundled coding_tdd.yaml's intent: test nodes scheduled
// before the implementation nodes they cover, and workers importing from
// the module under test rather than reimplementing it.

#[test]
fn coding_tdd_planner_producer_prompt_requires_test_nodes_before_implementation() {
    let policy = load_adapter(&repo_adapter("coding_tdd.yaml"))
        .unwrap()
        .role_policy();
    let required_substrings = ["before the implementation nodes", "name the source module"];
    for substring in required_substrings {
        assert!(
            policy.planner_producer_system.contains(substring),
            "TDD planner prompt must contain {substring:?}; got:\n{}",
            policy.planner_producer_system
        );
    }
}

#[test]
fn coding_tdd_worker_producer_prompt_requires_importing_functions_under_test() {
    let policy = load_adapter(&repo_adapter("coding_tdd.yaml"))
        .unwrap()
        .role_policy();
    assert!(
        policy
            .worker_producer_system
            .contains("import the functions under test"),
        "TDD worker prompt must require importing functions under test; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn coding_tdd_context_file_names_includes_readme() {
    let adapter = load_adapter(&repo_adapter("coding_tdd.yaml")).unwrap();
    assert!(
        adapter
            .context_file_names()
            .contains(&"README.md".to_string())
    );
}
