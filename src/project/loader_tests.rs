use super::*;
use crate::project::ProjectAdapter;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh, never-before-used adapters directory path for one test. The
/// directory itself is created lazily by whatever the test does with it.
fn unique_dir() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "forge-rs-adapters-test-{}-{id}",
        std::process::id()
    ))
}

const CUSTOM_ADAPTER_YAML: &str = r#"
role_prompts:
  planner_producer:
    instructions: "custom planner instructions"
    constraints: "custom planner constraints"
  worker_producer:
    instructions: "custom worker instructions"
    constraints: "custom worker constraints"
  planner_critic:
    instructions: "custom critic instructions"
    constraints: "custom critic constraints"
  worker_critic:
    instructions: "custom worker critic instructions"
    constraints: "custom worker critic constraints"
  planner_referee:
    instructions: "custom referee instructions"
    constraints: "custom referee constraints"
  worker_referee:
    instructions: "custom worker referee instructions"
    constraints: "custom worker referee constraints"
"#;

// ── built-in bootstrap ───────────────────────────────────────────────────

#[test]
fn coding_adapter_is_written_to_the_adapters_dir_on_first_use() {
    // Invariant: requesting a built-in adapter that isn't yet on disk seeds
    // the adapters directory with it, so it becomes visible/editable there.
    let dir = unique_dir();
    let adapter = load_adapter(&dir, "coding.yaml").unwrap();
    assert!(
        dir.join("coding.yaml").is_file(),
        "coding.yaml must be written to the adapters dir on first use"
    );
    assert!(!adapter.role_policy().planner_producer_system.is_empty());
}

#[test]
fn existing_adapter_file_is_not_overwritten_by_the_builtin_seed() {
    // Invariant: a user who has edited a built-in adapter's on-disk copy
    // must have their edits loaded, not silently clobbered by the bundled
    // seed content.
    let dir = unique_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("coding.yaml"), CUSTOM_ADAPTER_YAML).unwrap();

    let adapter = load_adapter(&dir, "coding.yaml").unwrap();
    assert!(
        adapter
            .role_policy()
            .planner_producer_system
            .contains("custom planner instructions"),
        "must load the on-disk content rather than the bundled seed"
    );
}

// ── user-defined adapters ────────────────────────────────────────────────

#[test]
fn user_defined_adapter_loads_from_disk_with_no_rust_changes() {
    let dir = unique_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("my_project.yaml"), CUSTOM_ADAPTER_YAML).unwrap();

    let adapter = load_adapter(&dir, "my_project.yaml").unwrap();
    assert!(
        adapter
            .role_policy()
            .worker_producer_system
            .contains("custom worker instructions")
    );
}

#[test]
fn unknown_adapter_is_a_hard_error() {
    let dir = unique_dir();
    let result = load_adapter(&dir, "bogus.yaml");
    assert!(result.is_err(), "unrecognised adapter must be a hard error");
}

// ── coding_tdd adapter content ───────────────────────────────────────────
//
// These protect the bundled coding_tdd.yaml's intent: test nodes scheduled
// before the implementation nodes they cover, and workers importing from
// the module under test rather than reimplementing it.

#[test]
fn coding_tdd_planner_producer_prompt_requires_test_nodes_before_implementation() {
    let dir = unique_dir();
    let policy = load_adapter(&dir, "coding_tdd.yaml").unwrap().role_policy();
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
    let dir = unique_dir();
    let policy = load_adapter(&dir, "coding_tdd.yaml").unwrap().role_policy();
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
    let dir = unique_dir();
    let adapter = load_adapter(&dir, "coding_tdd.yaml").unwrap();
    assert!(
        adapter
            .context_file_names()
            .contains(&"README.md".to_string())
    );
}
