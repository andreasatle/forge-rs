use super::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_config_path() -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "forge-config-test-{}-{}.yaml",
        std::process::id(),
        id,
    ))
}

struct TempYaml(PathBuf);

impl TempYaml {
    fn new(content: &str) -> Self {
        let path = unique_config_path();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempYaml {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
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
"#;

#[test]
fn parses_objective() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.objective,
        "Write a short haiku about Rust state machines."
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
        config.provider.timeout_seconds, 120,
        "absent timeout_seconds must default to 120"
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

// ── project config tests ─────────────────────────────────────────────────

#[test]
fn config_defaults_to_default_project() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.project.kind,
        ProjectKind::Default,
        "absent project block must default to ProjectKind::Default"
    );
}

const CODING_PROJECT_YAML: &str = r#"
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
project:
  kind: coding
"#;

#[test]
fn config_parses_coding_project() {
    let tmp = TempYaml::new(CODING_PROJECT_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.project.kind,
        ProjectKind::Coding,
        "project.kind: coding must parse as ProjectKind::Coding"
    );
}

#[test]
fn config_defaults_to_coding_variant() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.project.variant,
        ProjectVariant::Coding,
        "absent project.variant must default to ProjectVariant::Coding"
    );
}

const CODING_TDD_VARIANT_YAML: &str = r#"
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
project:
  kind: coding
  variant: coding_tdd
"#;

#[test]
fn config_parses_coding_tdd_variant() {
    let tmp = TempYaml::new(CODING_TDD_VARIANT_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.project.variant,
        ProjectVariant::CodingTdd,
        "project.variant: coding_tdd must parse as ProjectVariant::CodingTdd"
    );
}

const UNKNOWN_VARIANT_YAML: &str = r#"
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
project:
  kind: coding
  variant: bogus
"#;

#[test]
fn unknown_variant_fails_loudly() {
    let tmp = TempYaml::new(UNKNOWN_VARIANT_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "unrecognised project.variant must be a hard error"
    );
}

// ── language config tests ────────────────────────────────────────────────

const RUST_LANGUAGE_YAML: &str = r#"
objective: "implement a CLI tool"
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
project:
  language: rust
"#;

#[test]
fn config_parses_project_language_rust() {
    let tmp = TempYaml::new(RUST_LANGUAGE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.project.language.as_deref(),
        Some("rust"),
        "project.language: rust must parse as Some(\"rust\")"
    );
}

#[test]
fn config_language_defaults_to_none() {
    let tmp = TempYaml::new(EXAMPLE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert!(
        config.project.language.is_none(),
        "absent project.language must default to None"
    );
}

const PYTHON_LANGUAGE_YAML: &str = r#"
objective: "implement a CLI tool"
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
project:
  language: python
"#;

#[test]
fn config_parses_project_language_python() {
    let tmp = TempYaml::new(PYTHON_LANGUAGE_YAML);
    let config = ForgeConfig::from_file(tmp.path()).unwrap();
    assert_eq!(
        config.project.language.as_deref(),
        Some("python"),
        "project.language: python must parse as Some(\"python\")"
    );
}

const UNKNOWN_LANGUAGE_YAML: &str = r#"
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
project:
  language: cobol
"#;

#[test]
fn unknown_language_fails_loudly() {
    let tmp = TempYaml::new(UNKNOWN_LANGUAGE_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(result.is_err(), "unknown language must be a hard error");
}

const LANGUAGE_AND_VALIDATION_YAML: &str = r#"
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
project:
  language: rust
validation:
  commands:
    - cargo test
"#;

#[test]
fn language_and_validation_commands_are_mutually_exclusive() {
    let tmp = TempYaml::new(LANGUAGE_AND_VALIDATION_YAML);
    let result = ForgeConfig::from_file(tmp.path());
    assert!(
        result.is_err(),
        "specifying both project.language and validation must be an error"
    );
}
