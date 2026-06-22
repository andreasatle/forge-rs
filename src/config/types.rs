//! Forge configuration types loaded from a YAML file.

use serde::Deserialize;

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
}

/// Artifact repository configuration.
#[derive(Debug, Deserialize)]
pub struct ArtifactConfig {
    /// Path to the bare git repository that stores artifact commits.
    pub repo_path: String,
    /// Branch name used within the repository.
    pub branch: String,
}

/// LLM provider configuration.
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    /// Base URL of the provider server (e.g. `"http://localhost:8080"`).
    pub base_url: String,
    /// Maximum tokens to predict per completion call.
    pub n_predict: usize,
}

/// Telemetry output configuration.
#[derive(Debug, Deserialize)]
pub struct TelemetryConfig {
    /// Directory path where telemetry files will be written.
    pub directory: String,
}

impl ForgeConfig {
    /// Load a `ForgeConfig` from a YAML file at `path`.
    pub fn from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: ForgeConfig = serde_yaml::from_str(&content)?;
        Ok(config)
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
  directory: "runs/latest"
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
        assert_eq!(config.artifact.repo_path, ".forge/artifacts/main.git");
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
    fn parses_telemetry_config() {
        let tmp = TempYaml::new(EXAMPLE_YAML);
        let config = ForgeConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.telemetry.directory, "runs/latest");
    }

    #[test]
    fn missing_file_returns_error() {
        let result = ForgeConfig::from_file("/tmp/forge-nonexistent-config-test.yaml");
        assert!(result.is_err(), "missing file must return an error");
    }

    #[test]
    fn invalid_yaml_returns_error() {
        let tmp = TempYaml::new("not: valid: yaml: [");
        let result = ForgeConfig::from_file(tmp.path());
        assert!(result.is_err(), "invalid YAML must return an error");
    }
}
