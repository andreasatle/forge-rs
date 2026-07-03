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
#[path = "types_tests.rs"]
mod tests;
