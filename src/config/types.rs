//! Forge configuration types loaded from a YAML file.

use std::path::Path;

use serde::Deserialize;

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

/// LLM provider configuration.
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    /// Base URL of the provider server (e.g. `"http://localhost:8080"`).
    pub base_url: String,
    /// Maximum tokens to predict per completion call.
    pub n_predict: usize,
    /// HTTP request timeout in seconds. Absent configs default to 120.
    #[serde(default = "default_provider_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Base URL for the strong-tier provider. When absent, the strong tier
    /// falls back to `base_url`.
    #[serde(default)]
    pub strong_base_url: Option<String>,
    /// Maximum tokens for strong-tier completions. Falls back to `n_predict`
    /// when absent.
    #[serde(default)]
    pub strong_n_predict: Option<usize>,
    /// Timeout for strong-tier completions in seconds. Falls back to
    /// `timeout_seconds` when absent.
    #[serde(default)]
    pub strong_timeout_seconds: Option<u64>,
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

        Ok(config)
    }
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
  base_url: "http://localhost:8080"
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
        assert_eq!(config.provider.base_url, "http://localhost:8080");
        assert_eq!(config.provider.n_predict, 512);
    }

    #[test]
    fn provider_timeout_is_optional() {
        let tmp = TempYaml::new(EXAMPLE_YAML);
        let config = ForgeConfig::from_file(tmp.path()).unwrap();
        // EXAMPLE_YAML has no timeout_seconds — must still deserialize successfully.
        let _ = config.provider.timeout_seconds;
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
  base_url: "http://localhost:8080"
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
  base_url: "http://localhost:8080"
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
  base_url: "http://localhost:8080"
  n_predict: 512
  strong_base_url: "http://localhost:8081"
  strong_n_predict: 1024
  strong_timeout_seconds: 180
telemetry:
  directory: "runs"
"#;

    #[test]
    fn config_parses_optional_strong_provider_fields() {
        let tmp = TempYaml::new(STRONG_PROVIDER_YAML);
        let config = ForgeConfig::from_file(tmp.path()).unwrap();
        assert_eq!(
            config.provider.strong_base_url.as_deref(),
            Some("http://localhost:8081")
        );
        assert_eq!(config.provider.strong_n_predict, Some(1024));
        assert_eq!(config.provider.strong_timeout_seconds, Some(180));
    }

    #[test]
    fn strong_provider_fields_absent_defaults_to_none() {
        let tmp = TempYaml::new(EXAMPLE_YAML);
        let config = ForgeConfig::from_file(tmp.path()).unwrap();
        assert!(config.provider.strong_base_url.is_none());
        assert!(config.provider.strong_n_predict.is_none());
        assert!(config.provider.strong_timeout_seconds.is_none());
    }

    const ABSOLUTE_YAML: &str = r#"
objective: "test absolute paths"
artifact:
  repo_path: "/absolute/path/main.git"
  branch: "main"
provider:
  base_url: "http://localhost:8080"
  n_predict: 512
telemetry:
  directory: "/absolute/telemetry"
"#;

    #[test]
    fn relative_artifact_path_resolves_against_config_dir() {
        let tmp = TempYaml::new(EXAMPLE_YAML);
        let config = ForgeConfig::from_file(tmp.path()).unwrap();
        let config_dir = std::path::Path::new(tmp.path()).parent().unwrap();
        let expected = config_dir
            .join(".forge/artifacts/main.git")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            config.artifact.repo_path, expected,
            "relative artifact path must resolve against config file directory"
        );
    }

    #[test]
    fn relative_telemetry_path_resolves_against_config_dir() {
        let tmp = TempYaml::new(EXAMPLE_YAML);
        let config = ForgeConfig::from_file(tmp.path()).unwrap();
        let config_dir = std::path::Path::new(tmp.path()).parent().unwrap();
        let expected = config_dir.join("runs").to_string_lossy().into_owned();
        assert_eq!(
            config.telemetry.directory, expected,
            "relative telemetry directory must resolve against config file directory"
        );
    }

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
  base_url: "http://localhost:8080"
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
    fn existing_configs_still_parse() {
        let tmp = TempYaml::new(EXAMPLE_YAML);
        let result = ForgeConfig::from_file(tmp.path());
        assert!(
            result.is_ok(),
            "existing config without project: must still parse; got: {:?}",
            result.err()
        );
    }

    // ── language config tests ────────────────────────────────────────────────

    const RUST_LANGUAGE_YAML: &str = r#"
objective: "implement a CLI tool"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  base_url: "http://localhost:8080"
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

    const UNKNOWN_LANGUAGE_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  base_url: "http://localhost:8080"
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
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unknown language"),
            "error must mention unknown language; got: {msg}"
        );
        assert!(
            msg.contains("cobol"),
            "error must name the unknown language; got: {msg}"
        );
    }

    const LANGUAGE_AND_VALIDATION_YAML: &str = r#"
objective: "test"
artifact:
  repo_path: ".forge/artifacts/main.git"
  branch: "main"
provider:
  base_url: "http://localhost:8080"
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
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "error must mention mutual exclusion; got: {msg}"
        );
    }
}
