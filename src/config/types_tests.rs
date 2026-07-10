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

/// Copies a built-in adapter/plugin YAML from this crate's `adapters/` or
/// `plugins/` directory into `dir` (as `name`), so config fixtures that
/// reference it by a bare relative filename (e.g. `adapter: coding.yaml`)
/// resolve correctly against the temp directory holding the config file.
/// A no-op if already staged.
///
/// Adapter content is staged flat alongside its plugins (not nested under
/// `adapters/`/`plugins/` subdirectories), so the shipped adapters'
/// `../plugins/...` plugin paths are rewritten to bare filenames to match —
/// otherwise they'd resolve outside the isolated temp dir.
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
        for name in ["coding.yaml", "coding_tdd.yaml"] {
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
    northstar: northstars/project.md
    adapter: adapters/planner.yaml
    trigger: start
  - name: implement
    northstar: northstars/implementation.md
    adapter: adapters/implementation.yaml
    trigger: after_each(planner)
"#;

#[test]
fn parses_teams() {
    let tmp = TempYaml::new(TEAMS_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(config.teams.len(), 2);

    let planner = &config.teams[0];
    assert_eq!(planner.name, "planner");
    assert_eq!(planner.northstar, "northstars/project.md");
    assert_eq!(planner.adapter, "adapters/planner.yaml");
    assert_eq!(planner.trigger, "start");

    let implement = &config.teams[1];
    assert_eq!(implement.name, "implement");
    assert_eq!(implement.trigger, "after_each(planner)");
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

const CODING_TDD_ADAPTER_YAML: &str = r#"
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
adapter: coding_tdd.yaml
"#;

#[test]
fn config_parses_adapter() {
    let tmp = TempYaml::new(CODING_TDD_ADAPTER_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    let config_dir = std::path::Path::new(tmp.path()).parent().unwrap();
    let expected = config_dir
        .join("coding_tdd.yaml")
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
            .join("adapters")
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
        .join("adapters")
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
