//! YAML-deserializable configuration for [`super::YamlProjectAdapter`].

use serde::Deserialize;

/// Per-role system prompt strings loaded from YAML.
///
/// Mirrors [`crate::roles::RolePolicy`] field-for-field so a
/// [`ProjectAdapterConfig`] can populate a full role policy without any
/// prompt text hardcoded in Rust.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePromptsConfig {
    /// System instruction for the Plan-node Producer role.
    pub planner_producer: String,
    /// System instruction for the Work-node Producer role.
    pub worker_producer: String,
    /// System instruction for the Plan-node Critic role.
    pub planner_critic: String,
    /// System instruction for the Work-node Critic role.
    pub worker_critic: String,
    /// System instruction for the Plan-node Referee role.
    pub planner_referee: String,
    /// System instruction for the Work-node Referee role.
    pub worker_referee: String,
}

/// Full YAML-deserializable configuration for a [`super::YamlProjectAdapter`].
///
/// Covers everything [`crate::project::CodingProjectAdapter`] currently
/// hardcodes: role prompts and ambient context file names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAdapterConfig {
    /// Per-role system prompts.
    pub role_prompts: RolePromptsConfig,
    /// Artifact file names included as ambient context in prompts.
    #[serde(default)]
    pub context_files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
role_prompts:
  planner_producer: "plan it"
  worker_producer: "build it"
  planner_critic: "review the plan"
  worker_critic: "review the work"
  planner_referee: "decide the plan"
  worker_referee: "decide the work"
"#;

    // ── parsing ───────────────────────────────────────────────────────────────

    #[test]
    fn parses_role_prompts() {
        // Invariant: each role prompt field round-trips from YAML unchanged.
        let config: ProjectAdapterConfig = serde_yaml::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(config.role_prompts.planner_producer, "plan it");
        assert_eq!(config.role_prompts.worker_producer, "build it");
        assert_eq!(config.role_prompts.planner_critic, "review the plan");
        assert_eq!(config.role_prompts.worker_critic, "review the work");
        assert_eq!(config.role_prompts.planner_referee, "decide the plan");
        assert_eq!(config.role_prompts.worker_referee, "decide the work");
    }

    #[test]
    fn context_files_defaults_to_empty() {
        // Invariant: a config with only role_prompts still parses, with
        // context_files defaulting to an empty list.
        let config: ProjectAdapterConfig = serde_yaml::from_str(MINIMAL_YAML).unwrap();
        assert!(config.context_files.is_empty());
    }

    #[test]
    fn parses_context_files() {
        let yaml = format!("{MINIMAL_YAML}\ncontext_files:\n  - README.md\n");
        let config: ProjectAdapterConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config.context_files, vec!["README.md".to_string()]);
    }

    #[test]
    fn missing_role_prompts_field_is_an_error() {
        // Invariant: role_prompts is required — a missing sub-field fails to parse.
        let yaml = r#"
role_prompts:
  planner_producer: "plan it"
  worker_producer: "build it"
  planner_critic: "review the plan"
  worker_critic: "review the work"
  planner_referee: "decide the plan"
"#;
        let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "missing worker_referee must fail to parse");
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        // Invariant: unknown fields are hard errors, not silently ignored.
        let yaml = format!("{MINIMAL_YAML}\nbogus_field: true\n");
        let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(&yaml);
        assert!(result.is_err(), "unknown top-level field must be rejected");
    }

    #[test]
    fn missing_role_prompts_block_is_an_error() {
        let yaml = "context_files:\n  - README.md\n";
        let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "role_prompts must be required");
    }
}
