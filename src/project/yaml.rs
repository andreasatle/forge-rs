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
#[derive(Debug)]
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
        let planner = &self.config.planner;
        // RolePolicy has not yet been wired to dispatch per worker role, so
        // every Work node is still rendered with a single Producer/Critic/
        // Referee prompt. Until that dispatch exists, the first configured
        // worker role stands in for "the" worker prompt.
        let worker = self
            .config
            .workers
            .first()
            .expect("adapter config must define at least one worker role");
        RolePolicy {
            planner_producer_system: format!(
                "Role:\n{PLANNER_PRODUCER_IDENTITY}\n{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}",
                render_role_prompt(&planner.producer)
            ),
            worker_producer_system: format!(
                "Role:\n{WORKER_PRODUCER_IDENTITY}\n{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{WORK_PRODUCER_SYSTEM}",
                render_role_prompt(&worker.producer)
            ),
            planner_critic_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&planner.critic)
            ),
            worker_critic_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&worker.critic)
            ),
            planner_referee_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&planner.referee)
            ),
            worker_referee_system: format!(
                "{}\nConstraints:\n{GENERIC_CONSTRAINTS}\n{DEFAULT_SYSTEM}",
                render_role_prompt(&worker.referee)
            ),
            planner_protocol_schema: PLANNER_PROTOCOL_FOOTER_WITH_OPERATION.to_string(),
            language_guidance: None,
            language_constraints: None,
            worker_role_descriptions: self
                .config
                .workers
                .iter()
                .map(|w| (w.role.clone(), w.description.clone()))
                .collect(),
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
    use crate::project::yaml_config::{PlannerConfig, WorkerRoleConfig};

    fn prompt(instructions: &str, constraints: &str) -> RolePromptConfig {
        RolePromptConfig {
            instructions: instructions.to_string(),
            constraints: constraints.to_string(),
        }
    }

    fn planner_config() -> PlannerConfig {
        PlannerConfig {
            producer: prompt("plan it", "plan bounds"),
            critic: prompt("review the plan", "review plan bounds"),
            referee: prompt("decide the plan", "decide plan bounds"),
        }
    }

    fn worker_configs() -> Vec<WorkerRoleConfig> {
        vec![WorkerRoleConfig {
            role: "implementer".to_string(),
            description: "Implements code changes.".to_string(),
            producer: prompt("build it", "build bounds"),
            critic: prompt("review the work", "review work bounds"),
            referee: prompt("decide the work", "decide work bounds"),
        }]
    }

    fn adapter() -> YamlProjectAdapter {
        YamlProjectAdapter::new(ProjectAdapterConfig {
            planner: planner_config(),
            workers: worker_configs(),
            context_files: vec!["README.md".to_string()],
        })
    }

    // ── role_policy ───────────────────────────────────────────────────────────

    /// Assert that each needle occurs in `haystack`, in the given order,
    /// without requiring the text between or around them to match exactly —
    /// so unrelated formatting changes to the framework constants being
    /// composed don't break the test.
    fn assert_ordered_sections(haystack: &str, needles: &[&str]) {
        let mut search_from = 0;
        for needle in needles {
            let found_at = haystack[search_from..].find(needle).unwrap_or_else(|| {
                panic!(
                    "expected {needle:?} to appear at or after position {search_from} in:\n{haystack}"
                )
            });
            search_from += found_at + needle.len();
        }
    }

    #[test]
    fn role_policy_maps_each_field_from_config() {
        // Invariant: every RolePolicy field is composed from the matching
        // config field's instructions and constraints, rendered as separate
        // labeled sections, followed by the generic Constraints: section and
        // the shared framework protocol constants, in that order — with no
        // field left hardcoded or swapped. Worker fields come from the first
        // configured worker role.
        let policy = adapter().role_policy();

        assert_ordered_sections(
            &policy.planner_producer_system,
            &[
                "Role:",
                PLANNER_PRODUCER_IDENTITY,
                "Instructions:",
                "plan it",
                "Constraints:",
                "plan bounds",
                "Constraints:",
                GENERIC_CONSTRAINTS,
                PLANNER_PROTOCOL_FOOTER_WITH_OPERATION,
            ],
        );
        assert_ordered_sections(
            &policy.worker_producer_system,
            &[
                "Role:",
                WORKER_PRODUCER_IDENTITY,
                "Instructions:",
                "build it",
                "Constraints:",
                "build bounds",
                "Constraints:",
                GENERIC_CONSTRAINTS,
                WORK_PRODUCER_SYSTEM,
            ],
        );
        for (system, instructions, constraints) in [
            (
                &policy.planner_critic_system,
                "review the plan",
                "review plan bounds",
            ),
            (
                &policy.worker_critic_system,
                "review the work",
                "review work bounds",
            ),
            (
                &policy.planner_referee_system,
                "decide the plan",
                "decide plan bounds",
            ),
            (
                &policy.worker_referee_system,
                "decide the work",
                "decide work bounds",
            ),
        ] {
            assert_ordered_sections(
                system,
                &[
                    "Instructions:",
                    instructions,
                    "Constraints:",
                    constraints,
                    "Constraints:",
                    GENERIC_CONSTRAINTS,
                    DEFAULT_SYSTEM,
                ],
            );
        }
    }

    #[test]
    fn role_policy_uses_first_worker_role_when_multiple_are_configured() {
        // Invariant: with more than one worker role configured, role_policy
        // sources the (not-yet-per-role) worker prompt from the first entry,
        // not the last.
        let mut workers = worker_configs();
        workers.push(WorkerRoleConfig {
            role: "tester".to_string(),
            description: "Writes tests.".to_string(),
            producer: prompt("test it", "test bounds"),
            critic: prompt("review the tests", "review test bounds"),
            referee: prompt("decide the tests", "decide test bounds"),
        });
        let adapter = YamlProjectAdapter::new(ProjectAdapterConfig {
            planner: planner_config(),
            workers,
            context_files: vec![],
        });
        assert!(
            adapter
                .role_policy()
                .worker_producer_system
                .contains("build it"),
            "expected first worker role's prompt to be used"
        );
    }

    // ── context_file_names ────────────────────────────────────────────────────

    #[test]
    fn context_file_names_returns_configured_files() {
        let cases = [
            (vec!["README.md".to_string()], vec!["README.md".to_string()]),
            (vec![], vec![]),
        ];
        for (context_files, expected) in cases {
            let adapter = YamlProjectAdapter::new(ProjectAdapterConfig {
                planner: planner_config(),
                workers: worker_configs(),
                context_files,
            });
            assert_eq!(adapter.context_file_names(), expected);
        }
    }

    // ── from_yaml_str ─────────────────────────────────────────────────────────

    #[test]
    fn from_yaml_str_builds_working_adapter() {
        let yaml = r#"
planner:
  producer:
    instructions: "plan it"
    constraints: "plan bounds"
  critic:
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    instructions: "decide the plan"
    constraints: "decide plan bounds"
workers:
  - role: implementer
    description: "Implements code changes."
    producer:
      instructions: "build it"
      constraints: "build bounds"
    critic:
      instructions: "review the work"
      constraints: "review work bounds"
    referee:
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
