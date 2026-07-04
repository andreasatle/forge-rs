//! YAML-deserializable configuration for [`super::YamlProjectAdapter`].

use serde::Deserialize;

/// A role prompt split into its instructive and constraining halves.
///
/// `instructions` describes what the role must do; `constraints` bounds how
/// it may do it (prohibitions, rejection-grounding rules, scope limits).
/// [`super::YamlProjectAdapter::role_policy`] renders both as separate
/// labeled sections rather than concatenating them into one paragraph.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePromptConfig {
    /// What the role must do.
    pub instructions: String,
    /// Prohibitions and boundaries on how the role may do it.
    pub constraints: String,
}

/// Per-role system prompt strings loaded from YAML.
///
/// Mirrors [`crate::roles::RolePolicy`] field-for-field so a
/// [`ProjectAdapterConfig`] can populate a full role policy without any
/// prompt text hardcoded in Rust.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePromptsConfig {
    /// System instruction for the Plan-node Producer role.
    pub planner_producer: RolePromptConfig,
    /// System instruction for the Work-node Producer role.
    pub worker_producer: RolePromptConfig,
    /// System instruction for the Plan-node Critic role.
    pub planner_critic: RolePromptConfig,
    /// System instruction for the Work-node Critic role.
    pub worker_critic: RolePromptConfig,
    /// System instruction for the Plan-node Referee role.
    pub planner_referee: RolePromptConfig,
    /// System instruction for the Work-node Referee role.
    pub worker_referee: RolePromptConfig,
}

/// A worker role this project adapter defines, and which reduced or full
/// validation contract applies to nodes assigned that role.
///
/// `validation` is a project-adapter-facing label (e.g. `"ruff_only"`,
/// `"full"`); it documents the adapter's intent but is not resolved by this
/// struct. The framework assigns `role` to a node deterministically from the
/// node's target files (see [`crate::machines::scheduler::Node::worker_role`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    /// The worker role name (e.g. `"tester"`, `"implementer"`).
    pub role: String,
    /// Which validation contract this role runs (adapter-facing label only).
    pub validation: String,
}

/// Full YAML-deserializable configuration for a [`super::YamlProjectAdapter`].
///
/// Covers role prompts and ambient context file names for a project
/// adapter, whether built-in (`coding.yaml`, `coding_tdd.yaml`) or
/// user-defined.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAdapterConfig {
    /// Per-role system prompts.
    pub role_prompts: RolePromptsConfig,
    /// Artifact file names included as ambient context in prompts.
    #[serde(default)]
    pub context_files: Vec<String>,
    /// Worker roles this project adapter defines.
    #[serde(default)]
    pub workers: Vec<WorkerConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
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
"#;

    // ── parsing ───────────────────────────────────────────────────────────────

    #[test]
    fn parses_role_prompts() {
        // Invariant: each role prompt's instructions/constraints round-trip
        // from YAML unchanged.
        let config: ProjectAdapterConfig = serde_yaml::from_str(MINIMAL_YAML).unwrap();
        assert_eq!(config.role_prompts.planner_producer.instructions, "plan it");
        assert_eq!(
            config.role_prompts.planner_producer.constraints,
            "plan bounds"
        );
        assert_eq!(config.role_prompts.worker_producer.instructions, "build it");
        assert_eq!(
            config.role_prompts.worker_producer.constraints,
            "build bounds"
        );
        assert_eq!(
            config.role_prompts.planner_critic.instructions,
            "review the plan"
        );
        assert_eq!(
            config.role_prompts.planner_critic.constraints,
            "review plan bounds"
        );
        assert_eq!(
            config.role_prompts.worker_critic.instructions,
            "review the work"
        );
        assert_eq!(
            config.role_prompts.worker_critic.constraints,
            "review work bounds"
        );
        assert_eq!(
            config.role_prompts.planner_referee.instructions,
            "decide the plan"
        );
        assert_eq!(
            config.role_prompts.planner_referee.constraints,
            "decide plan bounds"
        );
        assert_eq!(
            config.role_prompts.worker_referee.instructions,
            "decide the work"
        );
        assert_eq!(
            config.role_prompts.worker_referee.constraints,
            "decide work bounds"
        );
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
        // Invariant: role_prompts, and each role prompt's instructions and
        // constraints sub-fields, are all required — a config missing any of
        // them fails to parse.
        let missing_worker_referee = r#"
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
"#;
        let missing_constraints_sub_field = r#"
role_prompts:
  planner_producer:
    instructions: "plan it"
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
"#;
        let missing_role_prompts_block = "context_files:\n  - README.md\n";

        let cases = [
            (missing_worker_referee, "missing worker_referee"),
            (
                missing_constraints_sub_field,
                "missing planner_producer.constraints",
            ),
            (missing_role_prompts_block, "missing role_prompts block"),
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
