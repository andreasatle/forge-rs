//! Project adapter driven entirely by a YAML-loaded [`ProjectAdapterConfig`].

use super::ProjectAdapter;
use super::yaml_config::{ProjectAdapterConfig, RolePromptConfig};
use crate::roles::RolePolicy;
use crate::roles::policy::{
    DEFAULT_SYSTEM, GENERIC_CONSTRAINTS, PLANNER_PRODUCER_IDENTITY,
    PLANNER_PROTOCOL_FOOTER_WITH_OPERATION, WORK_PRODUCER_SYSTEM, WORKER_PRODUCER_IDENTITY,
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
        // GENERIC_CONSTRAINTS renders as its own Constraints: section, after
        // the adapter's own Instructions:/Constraints: sections and before
        // the role's protocol-specific footer.
        let prompts = &self.config.role_prompts;
        RolePolicy {
            planner_producer_system: format!(
                "Role:\n{PLANNER_PRODUCER_IDENTITY}\n{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}",
                render_role_prompt(&prompts.planner_producer)
            ),
            worker_producer_system: format!(
                "Role:\n{WORKER_PRODUCER_IDENTITY}\n{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{WORK_PRODUCER_SYSTEM}",
                render_role_prompt(&prompts.worker_producer)
            ),
            planner_critic_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&prompts.planner_critic)
            ),
            worker_critic_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&prompts.worker_critic)
            ),
            planner_referee_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&prompts.planner_referee)
            ),
            worker_referee_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&prompts.worker_referee)
            ),
            planner_protocol_schema: PLANNER_PROTOCOL_FOOTER_WITH_OPERATION.to_string(),
            language_guidance: None,
            language_constraints: None,
        }
    }

    fn context_file_names(&self) -> Vec<String> {
        self.config.context_files.clone()
    }
}

/// Render a role prompt's instructions and constraints as separate labeled
/// sections, rather than concatenating them into one undifferentiated
/// paragraph.
fn render_role_prompt(prompt: &RolePromptConfig) -> String {
    format!(
        "Instructions:\n{}\nConstraints:\n{}",
        prompt.instructions, prompt.constraints
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::yaml_config::{RolePromptConfig, RolePromptsConfig};

    fn prompt(instructions: &str, constraints: &str) -> RolePromptConfig {
        RolePromptConfig {
            instructions: instructions.to_string(),
            constraints: constraints.to_string(),
        }
    }

    fn role_prompts() -> RolePromptsConfig {
        RolePromptsConfig {
            planner_producer: prompt("plan it", "plan bounds"),
            worker_producer: prompt("build it", "build bounds"),
            planner_critic: prompt("review the plan", "review plan bounds"),
            worker_critic: prompt("review the work", "review work bounds"),
            planner_referee: prompt("decide the plan", "decide plan bounds"),
            worker_referee: prompt("decide the work", "decide work bounds"),
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
        // RolePromptsConfig field's instructions and constraints, rendered as
        // separate labeled sections, plus a distinct generic Constraints:
        // section and the shared framework protocol constants, with no field
        // left hardcoded or swapped.
        let policy = adapter().role_policy();
        assert_eq!(
            policy.planner_producer_system,
            format!(
                "Role:\n{PLANNER_PRODUCER_IDENTITY}\nInstructions:\nplan it\nConstraints:\nplan bounds\nConstraints:\n{GENERIC_CONSTRAINTS}\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}"
            )
        );
        assert_eq!(
            policy.worker_producer_system,
            format!(
                "Role:\n{WORKER_PRODUCER_IDENTITY}\nInstructions:\nbuild it\nConstraints:\nbuild bounds\nConstraints:\n{GENERIC_CONSTRAINTS}\n{WORK_PRODUCER_SYSTEM}"
            )
        );
        assert_eq!(
            policy.planner_critic_system,
            format!(
                "Instructions:\nreview the plan\nConstraints:\nreview plan bounds\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"
            )
        );
        assert_eq!(
            policy.worker_critic_system,
            format!(
                "Instructions:\nreview the work\nConstraints:\nreview work bounds\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"
            )
        );
        assert_eq!(
            policy.planner_referee_system,
            format!(
                "Instructions:\ndecide the plan\nConstraints:\ndecide plan bounds\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"
            )
        );
        assert_eq!(
            policy.worker_referee_system,
            format!(
                "Instructions:\ndecide the work\nConstraints:\ndecide work bounds\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}"
            )
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
  planner_producer:
    instructions: "plan it"
    constraints: "plan bounds"
  worker_producer:
    instructions: "build it"
    constraints: "build bounds"
  planner_critic:
    instructions: "review the plan"
    constraints: "review plan bounds"
  worker_critic:
    instructions: "review the work"
    constraints: "review work bounds"
  planner_referee:
    instructions: "decide the plan"
    constraints: "decide plan bounds"
  worker_referee:
    instructions: "decide the work"
    constraints: "decide work bounds"
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
