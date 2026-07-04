use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_path(name: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "forge-rs-plugin-test-{}-{id}-{name}",
        std::process::id()
    ))
}

/// Path to a built-in language plugin YAML shipped alongside the crate.
fn repo_plugin(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(name)
}

// ── built-in plugins ─────────────────────────────────────────────────────

#[test]
fn rust_plugin_loads_from_its_shipped_path() {
    let spec = load_plugin(&repo_plugin("rust.yaml")).unwrap();
    assert!(!spec.prompt_guidance.is_empty());
    assert!(!spec.constraints.is_empty());
    assert!(!spec.init.commands.is_empty());
    assert!(!spec.validation.commands.is_empty());
}

#[test]
fn python_plugin_loads_from_its_shipped_path() {
    let spec = load_plugin(&repo_plugin("python.yaml")).unwrap();
    assert!(!spec.prompt_guidance.is_empty());
    assert!(!spec.constraints.is_empty());
    assert!(!spec.init.commands.is_empty());
    assert!(!spec.validation.commands.is_empty());
}

#[test]
fn rust_init_contains_cargo_init_vcs_none() {
    let spec = load_plugin(&repo_plugin("rust.yaml")).unwrap();
    let cmd = &spec.init.commands[0];
    assert_eq!(cmd.program, "cargo", "init program must be cargo");
    assert!(
        cmd.args.iter().any(|a| a == "init"),
        "init args must include 'init'; got: {:?}",
        cmd.args
    );
    assert!(
        cmd.args
            .windows(2)
            .any(|w| w[0] == "--vcs" && w[1] == "none"),
        "init must pass --vcs none; got: {:?}",
        cmd.args
    );
    assert!(
        cmd.args.last() == Some(&".".to_string()),
        "init must target the current directory; got: {:?}",
        cmd.args
    );
}

#[test]
fn rust_validation_contains_fmt_check_check_test() {
    let spec = load_plugin(&repo_plugin("rust.yaml")).unwrap();
    let cmds = &spec.validation.commands;

    assert!(
        cmds.iter().all(|c| c.program == "cargo"),
        "all validation commands must use cargo; got: {cmds:?}"
    );

    let has_fmt_check = cmds
        .iter()
        .any(|c| c.args.contains(&"fmt".to_string()) && c.args.contains(&"--check".to_string()));
    assert!(
        has_fmt_check,
        "validation must include cargo fmt --check; got: {cmds:?}"
    );

    let has_check = cmds.iter().any(|c| c.args == vec!["check"]);
    assert!(
        has_check,
        "validation must include cargo check; got: {cmds:?}"
    );

    let has_test = cmds.iter().any(|c| c.args == vec!["test"]);
    assert!(
        has_test,
        "validation must include cargo test; got: {cmds:?}"
    );
}

#[test]
fn python_init_first_command_is_uv_init_vcs_none() {
    let spec = load_plugin(&repo_plugin("python.yaml")).unwrap();
    assert!(
        spec.init.commands.len() >= 2,
        "python init must have at least two commands; got: {:?}",
        spec.init.commands
    );
    let cmd = &spec.init.commands[0];
    assert_eq!(cmd.program, "uv", "first init program must be uv");
    assert_eq!(
        cmd.args,
        vec!["init", "--vcs", "none"],
        "first init args must be [init, --vcs, none]; got: {:?}",
        cmd.args
    );
}

#[test]
fn python_init_second_command_adds_dev_dependencies() {
    let spec = load_plugin(&repo_plugin("python.yaml")).unwrap();
    let cmd = &spec.init.commands[1];
    assert_eq!(cmd.program, "uv", "second init program must be uv");
    assert!(
        cmd.args.contains(&"add".to_string()),
        "second init args must include 'add'; got: {:?}",
        cmd.args
    );
    assert!(
        cmd.args.contains(&"--dev".to_string()),
        "second init must pass --dev; got: {:?}",
        cmd.args
    );
    for pkg in ["pytest", "ruff", "pyright"] {
        assert!(
            cmd.args.contains(&pkg.to_string()),
            "second init must add {pkg}; got: {:?}",
            cmd.args
        );
    }
}

#[test]
fn python_validation_contains_ruff_pyright_pytest() {
    let spec = load_plugin(&repo_plugin("python.yaml")).unwrap();
    let cmds = &spec.validation.commands;

    assert!(
        cmds.iter().all(|c| c.program == "uv"),
        "all python validation commands must use uv; got: {cmds:?}"
    );

    let has_ruff = cmds
        .iter()
        .any(|c| c.args.contains(&"ruff".to_string()) && c.args.contains(&"check".to_string()));
    assert!(
        has_ruff,
        "validation must include ruff check; got: {cmds:?}"
    );

    let has_pyright = cmds.iter().any(|c| c.args.contains(&"pyright".to_string()));
    assert!(
        has_pyright,
        "validation must include pyright; got: {cmds:?}"
    );

    let has_pytest = cmds.iter().any(|c| c.args.contains(&"pytest".to_string()));
    assert!(has_pytest, "validation must include pytest; got: {cmds:?}");
}

// ── user-defined plugins ─────────────────────────────────────────────────

#[test]
fn missing_plugin_file_is_a_hard_error() {
    let path = unique_path("cobol.yaml");
    let err = load_plugin(&path).unwrap_err();
    assert!(
        err.to_string().contains("failed to read plugin"),
        "missing plugin file must fail with a clear read error; got: {err}"
    );
}

#[test]
fn user_defined_plugin_loads_from_any_path_with_no_rust_changes() {
    let path = unique_path("cobol.yaml");
    fs::write(&path, CUSTOM_PLUGIN_YAML).unwrap();

    let spec = load_plugin(&path).unwrap();
    assert_eq!(spec.prompt_guidance, "custom guidance");
}

const CUSTOM_PLUGIN_YAML: &str = r#"
prompt_guidance: "custom guidance"
constraints: "custom constraints"
init:
  commands:
    - program: "echo"
      args: ["hello"]
  gitignore: []
validation:
  commands:
    - program: "echo"
      args: ["ok"]
      when_files_present: []
      scope: workspace
  validation_targets: []
  validation_node_commands: []
"#;

// ── language_spec bare-id convenience ────────────────────────────────────

#[test]
fn bare_id_language_spec_loads_correctly() {
    for language in ["rust", "python"] {
        let spec = language_spec(language).unwrap_or_else(|| panic!("{language} spec must load"));
        assert!(!spec.prompt_guidance.is_empty());
        assert!(!spec.constraints.is_empty());
    }
}

#[test]
fn unknown_language_returns_none() {
    assert!(language_spec("java").is_none(), "java must be unknown");
    assert!(language_spec("cobol").is_none(), "cobol must be unknown");
    assert!(language_spec("").is_none(), "empty string must be unknown");
}
