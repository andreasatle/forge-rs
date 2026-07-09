//! YAML-deserializable configuration for [`super::YamlProjectAdapter`].

use serde::Deserialize;

/// A role prompt split into four explicit sections: identity, context,
/// instructions, constraints.
///
/// [`super::YamlProjectAdapter::role_policy`] renders all four as separate
/// labeled sections rather than concatenating them into one paragraph.
pub use crate::roles::policy::RolePromptConfig;

/// Producer/Critic/Referee prompts for the planner deliberation, which
/// operates on the task graph itself rather than any single worker role.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannerConfig {
    /// System instruction for the Plan-node Producer.
    pub producer: RolePromptConfig,
    /// System instruction for the Plan-node Critic.
    pub critic: RolePromptConfig,
    /// System instruction for the Plan-node Referee.
    pub referee: RolePromptConfig,
}

/// A worker role this project adapter defines: its own Producer/Critic/
/// Referee prompts, plus a human-readable description of what the role is
/// for.
///
/// The planner assigns `role` to each task explicitly, choosing from the
/// worker roles described here (see
/// [`crate::node_runner::planner::PlannerTask::role`]). Which validation
/// contract a role runs is declared by the language plugin's per-role
/// validation, not by this struct.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRoleConfig {
    /// The worker role name (e.g. `"tester"`, `"implementer"`).
    pub role: String,
    /// Human-readable description of what this role is responsible for.
    pub description: String,
    /// System instruction for this role's Producer.
    pub producer: RolePromptConfig,
    /// System instruction for this role's Critic.
    pub critic: RolePromptConfig,
    /// System instruction for this role's Referee.
    pub referee: RolePromptConfig,
}

/// Full YAML-deserializable configuration for a [`super::YamlProjectAdapter`].
///
/// Covers the planner's role prompts, the worker roles this project defines,
/// and ambient context file names for a project adapter, whether built-in
/// (`coding.yaml`, `coding_tdd.yaml`) or user-defined.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAdapterConfig {
    /// Planner Producer/Critic/Referee prompts.
    pub planner: PlannerConfig,
    /// Worker roles this project adapter defines, each with its own
    /// Producer/Critic/Referee prompts.
    pub workers: Vec<WorkerRoleConfig>,
    /// Artifact file names included as ambient context in prompts.
    #[serde(default)]
    pub context_files: Vec<String>,
    /// Paths to language plugin YAML files this adapter supports, resolved
    /// relative to this adapter file's own directory. The framework selects
    /// which one applies to a given node from the file extensions of its
    /// target files — see [`crate::project::loader::load_adapter`].
    #[serde(default)]
    pub plugins: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
planner:
  producer:
    identity: "plan identity"
    context: "plan context"
    instructions: "plan it"
    constraints: "plan bounds"
  critic:
    identity: "plan critic identity"
    context: "plan critic context"
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    identity: "plan referee identity"
    context: "plan referee context"
    instructions: "decide the plan"
    constraints: "decide plan bounds"
workers:
  - role: implementer
    description: "Implements code changes."
    producer:
      identity: "build identity"
      context: "build context"
      instructions: "build it"
      constraints: "build bounds"
    critic:
      identity: "build critic identity"
      context: "build critic context"
      instructions: "review the work"
      constraints: "review work bounds"
    referee:
      identity: "build referee identity"
      context: "build referee context"
      instructions: "decide the work"
      constraints: "decide work bounds"
"#;

    // ── parsing ───────────────────────────────────────────────────────────────

    #[test]
    fn parses_planner_prompts() {
        // Invariant: each planner prompt's identity/context/instructions/
        // constraints round-trip from YAML unchanged.
        let config: ProjectAdapterConfig = serde_yaml::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(config.planner.producer.identity, "plan identity");
        assert_eq!(config.planner.producer.context, "plan context");
        assert_eq!(config.planner.producer.instructions, "plan it");
        assert_eq!(config.planner.producer.constraints, "plan bounds");
        assert_eq!(config.planner.critic.instructions, "review the plan");
        assert_eq!(config.planner.critic.constraints, "review plan bounds");
        assert_eq!(config.planner.referee.instructions, "decide the plan");
        assert_eq!(config.planner.referee.constraints, "decide plan bounds");
    }

    #[test]
    fn parses_worker_roles() {
        // Invariant: each worker role's name, description, and
        // identity/context/instructions/constraints round-trip from YAML
        // unchanged.
        let config: ProjectAdapterConfig = serde_yaml::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(config.workers.len(), 1);
        let implementer = &config.workers[0];
        assert_eq!(implementer.role, "implementer");
        assert_eq!(implementer.description, "Implements code changes.");
        assert_eq!(implementer.producer.identity, "build identity");
        assert_eq!(implementer.producer.context, "build context");
        assert_eq!(implementer.producer.instructions, "build it");
        assert_eq!(implementer.producer.constraints, "build bounds");
        assert_eq!(implementer.critic.instructions, "review the work");
        assert_eq!(implementer.critic.constraints, "review work bounds");
        assert_eq!(implementer.referee.instructions, "decide the work");
        assert_eq!(implementer.referee.constraints, "decide work bounds");
    }

    #[test]
    fn multiple_worker_roles_all_parse() {
        let yaml = format!(
            "{MINIMAL_YAML}\n  - role: tester\n    description: \"Writes tests.\"\n    producer:\n      identity: \"test identity\"\n      context: \"test context\"\n      instructions: \"test it\"\n      constraints: \"test bounds\"\n    critic:\n      identity: \"test critic identity\"\n      context: \"test critic context\"\n      instructions: \"review the tests\"\n      constraints: \"review test bounds\"\n    referee:\n      identity: \"test referee identity\"\n      context: \"test referee context\"\n      instructions: \"decide the tests\"\n      constraints: \"decide test bounds\"\n"
        );
        let config: ProjectAdapterConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config.workers.len(), 2);
        assert_eq!(config.workers[1].role, "tester");
    }

    #[test]
    fn context_files_field() {
        let cases = [
            (MINIMAL_YAML.to_string(), Vec::<String>::new()),
            (
                format!("{MINIMAL_YAML}\ncontext_files:\n  - README.md\n"),
                vec!["README.md".to_string()],
            ),
        ];
        for (yaml, expected) in cases {
            // Invariant: context_files defaults to empty when absent from
            // YAML, and round-trips unchanged when present.
            let config: ProjectAdapterConfig = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(config.context_files, expected);
        }
    }

    #[test]
    fn missing_required_field_is_an_error() {
        // Invariant: planner and workers, and each role prompt's identity,
        // context, instructions, and constraints sub-fields, are all
        // required — a config missing any of them fails to parse.
        let missing_worker_referee = r#"
planner:
  producer:
    identity: "plan identity"
    context: "plan context"
    instructions: "plan it"
    constraints: "plan bounds"
  critic:
    identity: "plan critic identity"
    context: "plan critic context"
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    identity: "plan referee identity"
    context: "plan referee context"
    instructions: "decide the plan"
    constraints: "decide plan bounds"
workers:
  - role: implementer
    description: "Implements code changes."
    producer:
      identity: "build identity"
      context: "build context"
      instructions: "build it"
      constraints: "build bounds"
    critic:
      identity: "build critic identity"
      context: "build critic context"
      instructions: "review the work"
      constraints: "review work bounds"
"#;
        let missing_constraints_sub_field = r#"
planner:
  producer:
    identity: "plan identity"
    context: "plan context"
    instructions: "plan it"
  critic:
    identity: "plan critic identity"
    context: "plan critic context"
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    identity: "plan referee identity"
    context: "plan referee context"
    instructions: "decide the plan"
    constraints: "decide plan bounds"
workers: []
"#;
        let missing_identity_sub_field = r#"
planner:
  producer:
    context: "plan context"
    instructions: "plan it"
    constraints: "plan bounds"
  critic:
    identity: "plan critic identity"
    context: "plan critic context"
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    identity: "plan referee identity"
    context: "plan referee context"
    instructions: "decide the plan"
    constraints: "decide plan bounds"
workers: []
"#;
        let missing_context_sub_field = r#"
planner:
  producer:
    identity: "plan identity"
    instructions: "plan it"
    constraints: "plan bounds"
  critic:
    identity: "plan critic identity"
    context: "plan critic context"
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    identity: "plan referee identity"
    context: "plan referee context"
    instructions: "decide the plan"
    constraints: "decide plan bounds"
workers: []
"#;
        let missing_workers_block = r#"
planner:
  producer:
    identity: "plan identity"
    context: "plan context"
    instructions: "plan it"
    constraints: "plan bounds"
  critic:
    identity: "plan critic identity"
    context: "plan critic context"
    instructions: "review the plan"
    constraints: "review plan bounds"
  referee:
    identity: "plan referee identity"
    context: "plan referee context"
    instructions: "decide the plan"
    constraints: "decide plan bounds"
"#;

        let cases = [
            (missing_worker_referee, "missing worker referee"),
            (
                missing_constraints_sub_field,
                "missing planner.producer.constraints",
            ),
            (
                missing_identity_sub_field,
                "missing planner.producer.identity",
            ),
            (
                missing_context_sub_field,
                "missing planner.producer.context",
            ),
            (missing_workers_block, "missing workers block"),
        ];
        for (yaml, description) in cases {
            let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(yaml);
            assert!(result.is_err(), "{description} must fail to parse");
        }
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        // Invariant: unknown fields are hard errors, not silently ignored.
        let yaml = format!("{MINIMAL_YAML}\nbogus_field: true\n");
        let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(&yaml);
        assert!(result.is_err(), "unknown top-level field must be rejected");
    }
}
