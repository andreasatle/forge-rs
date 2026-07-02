//! Project adapter driven entirely by a YAML-loaded [`ProjectAdapterConfig`].

use super::ProjectAdapter;
use super::yaml_config::ProjectAdapterConfig;
use crate::roles::RolePolicy;
use crate::roles::policy::{
    DEFAULT_SYSTEM, PLANNER_PRODUCER_IDENTITY, PLANNER_PROTOCOL_FOOTER_WITH_OPERATION,
    WORK_PRODUCER_SYSTEM, WORKER_PRODUCER_IDENTITY,
};

/// A [`ProjectAdapter`] whose role prompts and context files come from a
/// [`ProjectAdapterConfig`], rather than being hardcoded in Rust.
pub struct YamlProjectAdapter {
    config: ProjectAdapterConfig,
}

impl YamlProjectAdapter {
    /// Build an adapter from an already-parsed configuration.
    pub fn new(config: ProjectAdapterConfig) -> Self {
        Self { config }
    }

    /// Parse a [`ProjectAdapterConfig`] from a YAML string and build an adapter.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, serde_yaml::Error> {
        let config: ProjectAdapterConfig = serde_yaml::from_str(yaml)?;
        Ok(Self::new(config))
    }
}

impl ProjectAdapter for YamlProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        // Each YAML-configured prompt supplies only the project-specific
        // portion of the role's system prompt. The generic role-identity and
        // JSON-protocol portions are framework constants, composed here so
        // every adapter gets them uniformly without restating them in YAML.
        let prompts = &self.config.role_prompts;
        RolePolicy {
            planner_producer_system: format!(
                "{PLANNER_PRODUCER_IDENTITY}\n{}\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}",
                prompts.planner_producer
            ),
            worker_producer_system: format!(
                "{WORKER_PRODUCER_IDENTITY} {}\n{WORK_PRODUCER_SYSTEM}",
                prompts.worker_producer
            ),
            planner_critic_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.planner_critic),
            worker_critic_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.worker_critic),
            planner_referee_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.planner_referee),
            worker_referee_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.worker_referee),
            language_guidance: None,
        }
    }

    fn context_file_names(&self) -> Vec<String> {
        self.config.context_files.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::yaml_config::RolePromptsConfig;

    fn role_prompts() -> RolePromptsConfig {
        RolePromptsConfig {
            planner_producer: "plan it".to_string(),
            worker_producer: "build it".to_string(),
            planner_critic: "review the plan".to_string(),
            worker_critic: "review the work".to_string(),
            planner_referee: "decide the plan".to_string(),
            worker_referee: "decide the work".to_string(),
        }
    }

    fn adapter() -> YamlProjectAdapter {
        YamlProjectAdapter::new(ProjectAdapterConfig {
            role_prompts: role_prompts(),
            context_files: vec!["README.md".to_string()],
        })
    }

    // ── role_policy ───────────────────────────────────────────────────────────

    #[test]
    fn role_policy_maps_each_field_from_config() {
        // Invariant: every RolePolicy field is composed from the matching
        // RolePromptsConfig field plus the shared framework protocol
        // constants, with no field left hardcoded or swapped.
        let policy = adapter().role_policy();
        assert_eq!(
            policy.planner_producer_system,
            format!(
                "{PLANNER_PRODUCER_IDENTITY}\nplan it\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}"
            )
        );
        assert_eq!(
            policy.worker_producer_system,
            format!("{WORKER_PRODUCER_IDENTITY} build it\n{WORK_PRODUCER_SYSTEM}")
        );
        assert_eq!(
            policy.planner_critic_system,
            format!("review the plan\n{DEFAULT_SYSTEM}")
        );
        assert_eq!(
            policy.worker_critic_system,
            format!("review the work\n{DEFAULT_SYSTEM}")
        );
        assert_eq!(
            policy.planner_referee_system,
            format!("decide the plan\n{DEFAULT_SYSTEM}")
        );
        assert_eq!(
            policy.worker_referee_system,
            format!("decide the work\n{DEFAULT_SYSTEM}")
        );
    }

    // ── context_file_names ────────────────────────────────────────────────────

    #[test]
    fn context_file_names_returns_configured_files() {
        assert_eq!(
            adapter().context_file_names(),
            vec!["README.md".to_string()]
        );
    }

    #[test]
    fn context_file_names_empty_when_unconfigured() {
        let adapter = YamlProjectAdapter::new(ProjectAdapterConfig {
            role_prompts: role_prompts(),
            context_files: vec![],
        });
        assert!(adapter.context_file_names().is_empty());
    }

    // ── from_yaml_str ─────────────────────────────────────────────────────────

    #[test]
    fn from_yaml_str_builds_working_adapter() {
        let yaml = r#"
role_prompts:
  planner_producer: "plan it"
  worker_producer: "build it"
  planner_critic: "review the plan"
  worker_critic: "review the work"
  planner_referee: "decide the plan"
  worker_referee: "decide the work"
context_files:
  - README.md
"#;
        let adapter = YamlProjectAdapter::from_yaml_str(yaml).unwrap();
        assert!(
            adapter
                .role_policy()
                .planner_producer_system
                .contains("plan it")
        );
        assert_eq!(adapter.context_file_names(), vec!["README.md".to_string()]);
    }

    #[test]
    fn from_yaml_str_rejects_invalid_yaml() {
        let result = YamlProjectAdapter::from_yaml_str("not: valid: yaml: [");
        assert!(result.is_err(), "invalid YAML must return an error");
    }
}
