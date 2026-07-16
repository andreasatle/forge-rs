//! YAML-deserializable configuration for [`super::YamlProjectAdapter`].

use serde::Deserialize;

/// A role prompt split into four explicit sections: identity, context,
/// instructions, constraints.
///
/// [`crate::project::ProjectAdapter::role_policy`] renders all four as
/// separate labeled sections rather than concatenating them into one
/// paragraph.
pub use crate::roles::policy::RolePromptConfig;

/// Producer/Critic/Referee prompts for the planner deliberation, which
/// operates on the task graph itself rather than any single worker role.
///
/// Defaults to empty prompts: an adapter whose teams never drive a `Plan`
/// node (e.g. a Work-only adapter reached exclusively via `after_teams`
/// triggers) may omit `planner` entirely.
#[derive(Debug, Clone, Default, Deserialize)]
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
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRoleConfig {
    /// The worker role name (e.g. `"tester"`, `"implementer"`), matched
    /// against a language plugin's own `plugin_role` entries.
    ///
    /// Required whenever the adapter declares any language plugins — see
    /// `crate::node_runner::project_setup::validate_worker_roles`, which
    /// enforces this at config-load time since there is no other way to
    /// select this role's per-plugin validation override. May be omitted
    /// entirely when the adapter declares no plugins at all (e.g. a
    /// document-writing adapter with no language plugin to match against).
    #[serde(default)]
    pub plugin_role: Option<String>,
    /// Whether this role's target file for a `ForTasks`-spawned node is
    /// derived from the task's source `file_path` via the active language
    /// plugin's validation-target derivation (see
    /// [`crate::validation::ValidationTargetRule`]), rather than being
    /// `file_path` itself.
    ///
    /// Set by roles that produce a validation artifact for another role's
    /// source file (e.g. a `tester` role writing `tests/test_main.py` for
    /// an `implementer` role's `main.py`). Left `false` (the default) by the
    /// role that owns the source file directly.
    #[serde(default)]
    pub derives_target: bool,
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
/// (`planner.yaml`, `implement.yaml`, `create_test.yaml`, `pass_tests.yaml`)
/// or user-defined.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAdapterConfig {
    /// Planner Producer/Critic/Referee prompts.
    ///
    /// Omitted by adapters whose teams never drive a `Plan` node.
    #[serde(default)]
    pub planner: PlannerConfig,
    /// Worker roles this project adapter defines, each with its own
    /// Producer/Critic/Referee prompts.
    ///
    /// Omitted (or empty) by adapters whose teams only ever drive a `Plan`
    /// node, which never renders worker-role prompts. A Work-only adapter
    /// still needs exactly one entry here even without naming multiple
    /// roles: `workers.first()` supplies the shared Work-node prompts. An
    /// empty list falls back to empty prompts rather than panicking — see
    /// [`crate::project::ProjectAdapter::role_policy`].
    #[serde(default)]
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
  - plugin_role: implementer
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
        assert_eq!(implementer.plugin_role.as_deref(), Some("implementer"));
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
    fn plugin_role_defaults_to_none_when_omitted() {
        // Invariant: `plugin_role` is optional at the schema level — an
        // adapter with no plugins to match against may omit it entirely.
        // Whether it's actually required (because the adapter declares
        // plugins) is enforced separately by
        // `node_runner::project_setup::validate_worker_roles`, not by
        // parsing.
        let yaml = r#"
workers:
  - description: "Implements code changes."
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
        let config: ProjectAdapterConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.workers.len(), 1);
        assert_eq!(config.workers[0].plugin_role, None);
    }

    #[test]
    fn multiple_worker_roles_all_parse() {
        let yaml = format!(
            "{MINIMAL_YAML}\n  - plugin_role: tester\n    description: \"Writes tests.\"\n    producer:\n      identity: \"test identity\"\n      context: \"test context\"\n      instructions: \"test it\"\n      constraints: \"test bounds\"\n    critic:\n      identity: \"test critic identity\"\n      context: \"test critic context\"\n      instructions: \"review the tests\"\n      constraints: \"review test bounds\"\n    referee:\n      identity: \"test referee identity\"\n      context: \"test referee context\"\n      instructions: \"decide the tests\"\n      constraints: \"decide test bounds\"\n"
        );
        let config: ProjectAdapterConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config.workers.len(), 2);
        assert_eq!(config.workers[1].plugin_role.as_deref(), Some("tester"));
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
        // Invariant: `planner` and `workers` may each be omitted entirely
        // (see `workers_and_planner_may_both_be_omitted`), but a role prompt
        // that IS present still requires all four of its identity, context,
        // instructions, and constraints sub-fields, and a worker entry that
        // IS present still requires its own producer/critic/referee blocks.
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
  - plugin_role: implementer
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
        ];
        for (yaml, description) in cases {
            let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(yaml);
            assert!(result.is_err(), "{description} must fail to parse");
        }
    }

    #[test]
    fn workers_and_planner_may_both_be_omitted() {
        // Invariant: a single-purpose adapter whose teams drive only `Plan`
        // nodes needs no `workers`, and one whose teams drive only `Work`
        // nodes needs no `planner` — both fields default when absent rather
        // than failing to parse.
        let plan_only = r#"
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
        let work_only = r#"
workers:
  - plugin_role: implementer
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
        for (yaml, description) in [
            (plan_only, "planner without workers"),
            (work_only, "workers without planner"),
        ] {
            let result: Result<ProjectAdapterConfig, _> = serde_yaml::from_str(yaml);
            assert!(result.is_ok(), "{description} must parse: {result:?}");
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
