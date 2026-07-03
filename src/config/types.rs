//! Forge configuration types loaded from a YAML file.

use std::path::Path;

use serde::{Deserialize, Deserializer};

/// Selects which project adapter governs role prompt policy for a run.
#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    /// Default adapter — uses the hardcoded JSON protocol prompts unchanged.
    #[default]
    Default,
    /// Coding adapter — uses software-oriented role prompts.
    Coding,
}

/// Selects which bundled coding adapter configuration governs role prompts
/// when [`ProjectKind::Coding`] is selected.
#[derive(Debug, Deserialize, Default, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ProjectVariant {
    /// The standard coding adapter, loaded from `coding.yaml`.
    #[default]
    Coding,
    /// The test-driven-development coding adapter, loaded from `coding_tdd.yaml`.
    CodingTdd,
}

/// Project-level configuration.
#[derive(Debug, Deserialize, Default)]
pub struct ProjectConfig {
    /// Selects which project adapter to use. Defaults to [`ProjectKind::Default`].
    #[serde(default)]
    pub kind: ProjectKind,
    /// Language identifier used to load init and validation specs.
    ///
    /// When set, the named language spec provides init and validation commands.
    /// Mutually exclusive with an explicit `validation` block.
    #[serde(default)]
    pub language: Option<String>,
    /// Selects which bundled coding adapter configuration to use when
    /// `kind` is [`ProjectKind::Coding`]. Defaults to [`ProjectVariant::Coding`].
    /// An unrecognised value is a hard error at config load time.
    #[serde(default)]
    pub variant: ProjectVariant,
}

/// Top-level configuration for a forge run.
#[derive(Debug, Deserialize)]
pub struct ForgeConfig {
    /// The objective the run will work toward.
    pub objective: String,
    /// Artifact repository settings.
    pub artifact: ArtifactConfig,
    /// Provider settings.
    pub provider: ProviderConfig,
    /// Telemetry settings.
    pub telemetry: TelemetryConfig,
    /// Optional validation commands run after workspace update, before integration.
    #[serde(default)]
    pub validation: Option<ValidationConfig>,
    /// Project adapter selection. Absent config defaults to [`ProjectKind::Default`].
    #[serde(default)]
    pub project: ProjectConfig,
}

/// Artifact repository configuration.
#[derive(Debug, Deserialize)]
pub struct ArtifactConfig {
    /// Path to the bare git repository that stores artifact commits.
    pub repo_path: String,
    /// Branch name used within the repository.
    pub branch: String,
}

fn default_provider_timeout_seconds() -> u64 {
    120
}

fn default_managed_startup_timeout_seconds() -> u64 {
    60
}

/// LLM provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Cheap/default provider tier.
    pub cheap: ProviderTierConfig,
    /// Optional strong provider tier. When absent, strong falls back to cheap.
    #[serde(default)]
    pub strong: Option<ProviderTierConfig>,
    /// HTTP request timeout in seconds. Absent configs default to 120.
    #[serde(default = "default_provider_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Timeout for strong-tier completions in seconds. Falls back to
    /// `timeout_seconds` when absent.
    #[serde(default)]
    pub strong_timeout_seconds: Option<u64>,
}

/// Configuration for one provider tier.
#[derive(Debug, Clone)]
pub enum ProviderTierConfig {
    /// Forge connects to an already-running provider server.
    Unmanaged(UnmanagedProviderConfig),
    /// Forge owns the local provider server process.
    Managed(ManagedProviderConfig),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTierConfigDef {
    #[serde(default)]
    unmanaged: Option<UnmanagedProviderConfig>,
    #[serde(default)]
    managed: Option<ManagedProviderConfig>,
}

impl<'de> Deserialize<'de> for ProviderTierConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let def = ProviderTierConfigDef::deserialize(deserializer)?;
        match (def.unmanaged, def.managed) {
            (Some(config), None) => Ok(Self::Unmanaged(config)),
            (None, Some(config)) => Ok(Self::Managed(config)),
            (Some(_), Some(_)) => Err(serde::de::Error::custom(
                "provider tier must specify exactly one of unmanaged or managed",
            )),
            (None, None) => Err(serde::de::Error::custom(
                "provider tier must specify unmanaged or managed",
            )),
        }
    }
}

/// Already-running provider server configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmanagedProviderConfig {
    /// Base URL of the provider server (e.g. `"http://localhost:8080"`).
    pub base_url: String,
    /// Expected model served by the provider at `base_url`.
    pub model: String,
    /// Maximum tokens to predict per completion call.
    pub n_predict: usize,
}

/// Managed local provider server configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProviderConfig {
    /// llama.cpp server management settings.
    pub llama_cpp: ManagedLlamaCppConfig,
}

/// Managed llama.cpp `llama-server` process configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLlamaCppConfig {
    /// Executable path or command name for `llama-server`.
    pub command: String,
    /// Model source passed to `llama-server`.
    pub model: ManagedLlamaCppModelConfig,
    /// Host passed to `llama-server --host`.
    pub host: String,
    /// Port passed to `llama-server --port`.
    pub port: u16,
    /// Optional llama.cpp context size, passed as `--ctx-size`.
    #[serde(default)]
    pub context_size: Option<usize>,
    /// Seconds to wait for readiness after spawning the process.
    #[serde(default = "default_managed_startup_timeout_seconds")]
    pub startup_timeout_seconds: u64,
    /// Maximum tokens to predict per completion call.
    pub n_predict: usize,
}

/// Managed llama.cpp model source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedLlamaCppModelConfig {
    /// Local GGUF model path passed as `--model <path>`.
    Path(String),
    /// Hugging Face llama.cpp reference passed as `-hf <repo:quant>`.
    HuggingFace(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedLlamaCppModelConfigDef {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    hf: Option<String>,
}

impl<'de> Deserialize<'de> for ManagedLlamaCppModelConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ModelDef {
            LocalPathString(String),
            Source(ManagedLlamaCppModelConfigDef),
        }

        match ModelDef::deserialize(deserializer)? {
            ModelDef::LocalPathString(path) => Ok(Self::Path(path)),
            ModelDef::Source(def) => match (def.path, def.hf) {
                (Some(path), None) => Ok(Self::Path(path)),
                (None, Some(hf)) => Ok(Self::HuggingFace(hf)),
                (Some(_), Some(_)) => Err(serde::de::Error::custom(
                    "managed llama.cpp model must specify exactly one of path or hf",
                )),
                (None, None) => Err(serde::de::Error::custom(
                    "managed llama.cpp model must specify path or hf",
                )),
            },
        }
    }
}

impl ManagedLlamaCppModelConfig {
    /// User-facing model identity used in run metadata.
    pub fn identity(&self) -> &str {
        match self {
            Self::Path(path) => path,
            Self::HuggingFace(hf) => hf,
        }
    }
}

/// Telemetry output configuration.
#[derive(Debug, Deserialize)]
pub struct TelemetryConfig {
    /// Directory path where telemetry files will be written.
    pub directory: String,
}

/// Validation configuration for post-workspace-update checks.
#[derive(Debug, Deserialize)]
pub struct ValidationConfig {
    /// Shell commands run in order inside the workspace; stop on first failure.
    pub commands: Vec<String>,
    /// Maximum seconds to wait for each command. Stored but not yet enforced.
    pub timeout_seconds: Option<u64>,
}

impl ForgeConfig {
    /// Load a `ForgeConfig` from a YAML file at `path`.
    ///
    /// Relative paths in `artifact.repo_path` and `telemetry.directory` are
    /// resolved against the directory containing the config file, not the
    /// process working directory.
    ///
    /// Returns an error if:
    /// - Both `project.language` and `validation` are specified (mutually exclusive).
    /// - `project.language` names an unknown language.
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let config_path = Path::new(path);
        let content = std::fs::read_to_string(config_path)?;
        let mut config: ForgeConfig = serde_yaml::from_str(&content)?;

        // Resolve relative paths against the config file's directory so that
        // `forge run path/to/forge.yaml` works correctly from any cwd.
        let config_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(dir) = config_dir {
            config.artifact.repo_path = resolve_relative(&config.artifact.repo_path, dir);
            config.telemetry.directory = resolve_relative(&config.telemetry.directory, dir);
        }

        if config.project.language.is_some() && config.validation.is_some() {
            return Err(
                "project.language and validation.commands are mutually exclusive; \
                 remove one or the other"
                    .into(),
            );
        }

        if let Some(lang) = &config.project.language
            && crate::language::registry::language_spec(lang).is_none()
        {
            return Err(format!("unknown language: '{lang}'").into());
        }

        validate_provider_model_identity(&config.provider)?;

        Ok(config)
    }
}

fn validate_provider_model_identity(
    provider: &ProviderConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_provider_tier("provider.cheap", &provider.cheap)?;
    if let Some(strong) = &provider.strong {
        validate_provider_tier("provider.strong", strong)?;
    }
    if provider.timeout_seconds == 0 {
        return Err("provider.timeout_seconds must be positive".into());
    }
    if let Some(timeout) = provider.strong_timeout_seconds
        && timeout == 0
    {
        return Err("provider.strong_timeout_seconds must be positive when set".into());
    }
    Ok(())
}

fn validate_provider_tier(
    field: &str,
    tier: &ProviderTierConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match tier {
        ProviderTierConfig::Unmanaged(config) => {
            if config.base_url.trim().is_empty() {
                return Err(format!("{field}.unmanaged.base_url must be non-empty").into());
            }
            validate_http_url(&format!("{field}.unmanaged.base_url"), &config.base_url)?;
            if config.model.trim().is_empty() {
                return Err(format!("{field}.unmanaged.model must be non-empty").into());
            }
        }
        ProviderTierConfig::Managed(managed) => {
            let llama = &managed.llama_cpp;
            if llama.command.trim().is_empty() {
                return Err(format!("{field}.managed.llama_cpp.command must be non-empty").into());
            }
            if llama.port == 0 {
                return Err(format!("{field}.managed.llama_cpp.port must not be 0").into());
            }
            match &llama.model {
                ManagedLlamaCppModelConfig::Path(path) if path.trim().is_empty() => {
                    return Err(
                        format!("{field}.managed.llama_cpp.model.path must be non-empty").into(),
                    );
                }
                ManagedLlamaCppModelConfig::HuggingFace(hf) if hf.trim().is_empty() => {
                    return Err(
                        format!("{field}.managed.llama_cpp.model.hf must be non-empty").into(),
                    );
                }
                _ => {}
            }
            if llama.host.trim().is_empty() {
                return Err(format!("{field}.managed.llama_cpp.host must be non-empty").into());
            }
            if llama.startup_timeout_seconds == 0 {
                return Err(format!(
                    "{field}.managed.llama_cpp.startup_timeout_seconds must be positive"
                )
                .into());
            }
        }
    }
    Ok(())
}

fn validate_http_url(field: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let trimmed = value.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(format!(
            "{field} must be a valid URL with an http or https scheme (got '{value}')"
        )
        .into());
    };
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "{field} must use the http or https scheme, got '{scheme}' (in '{value}')"
        )
        .into());
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.trim().is_empty() {
        return Err(format!("{field} must include a host (got '{value}')").into());
    }
    Ok(())
}

fn resolve_relative(path_str: &str, base: &Path) -> String {
    let p = Path::new(path_str);
    if p.is_absolute() {
        path_str.to_string()
    } else {
        base.join(p).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
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
}
