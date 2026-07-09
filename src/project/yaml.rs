//! Project adapter driven entirely by a YAML-loaded [`ProjectAdapterConfig`].

use std::collections::BTreeMap;

use super::ProjectAdapter;
use super::yaml_config::{ProjectAdapterConfig, RolePromptConfig, WorkerRoleConfig};
use crate::language::LanguageSpec;
use crate::roles::RolePolicy;
use crate::roles::policy::{
    DEFAULT_SYSTEM, PLANNER_PRODUCER_IDENTITY, PLANNER_PROTOCOL_FOOTER_WITH_OPERATION,
    PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES, WORK_PRODUCER_SYSTEM,
    WORKER_PRODUCER_IDENTITY, WorkerRolePolicy, generic_prompt, render_role_prompt,
};

/// A [`ProjectAdapter`] whose role prompts and context files come from a
/// [`ProjectAdapterConfig`], rather than being hardcoded in Rust.
///
/// Role prompts never carry any language plugin's guidance — an adapter's
/// declared plugins vary by node (selected from each node's own target
/// files, see [`crate::language::select_plugin`]), not by adapter, so
/// [`ProjectAdapter::role_policy`] composes only the generic and adapter
/// layers here. The node runner injects the per-node plugin layer at prompt
/// render time instead.
#[derive(Debug)]
pub struct YamlProjectAdapter {
    config: ProjectAdapterConfig,
    /// This adapter's declared language plugins, keyed by each plugin's
    /// declared file extensions (see [`LanguageSpec::extensions`]). Loaded
    /// from `config.plugins` and attached via [`Self::with_language_plugins`]
    /// — see [`crate::project::loader::load_adapter`].
    language_plugins: BTreeMap<String, LanguageSpec>,
}

impl YamlProjectAdapter {
    /// Build an adapter from an already-parsed configuration, with no
    /// language plugin.
    pub fn new(config: ProjectAdapterConfig) -> Self {
        Self {
            config,
            language_plugins: BTreeMap::new(),
        }
    }

    /// Parse a [`ProjectAdapterConfig`] from a YAML string and build an adapter.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, serde_yaml::Error> {
        let config: ProjectAdapterConfig = serde_yaml::from_str(yaml)?;
        Ok(Self::new(config))
    }

    /// Attach this adapter's loaded language plugins, keyed by extension.
    pub fn with_language_plugins(
        mut self,
        language_plugins: BTreeMap<String, LanguageSpec>,
    ) -> Self {
        self.language_plugins = language_plugins;
        self
    }

    /// This adapter's loaded language plugins, keyed by extension.
    pub fn language_plugins(&self) -> &BTreeMap<String, LanguageSpec> {
        &self.language_plugins
    }

    /// Paths to this adapter's declared language plugin YAML files, as
    /// written in its `plugins:` list — resolved relative to the adapter
    /// file's own directory by [`crate::project::loader::load_adapter`].
    pub fn plugin_paths(&self) -> &[String] {
        &self.config.plugins
    }
}

impl ProjectAdapter for YamlProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        // Every role's system prompt composes three layers per section
        // (Identity/Context/Instructions/Constraints): the generic layer
        // (always present, see `generic_prompt`) and the adapter's own
        // per-role layer. The language plugin's layer is never baked in
        // here — the node runner selects and injects it per node from each
        // node's own target files (see `crate::language::select_plugin`),
        // since an adapter's declared plugins vary by node, not by adapter.
        let generic = generic_prompt();
        let planner = &self.config.planner;
        // The shared worker_*_system fields below fall back to the first
        // configured worker role; node_runner dispatch selects a Work node's
        // own role from worker_role_policies when its worker_role matches.
        let worker = self
            .config
            .workers
            .first()
            .expect("adapter config must define at least one worker role");
        // Adapters that define worker roles must have the planner assign one
        // to every task; the footer variant describes `role` as required
        // rather than optional to match that expectation.
        let planner_protocol_footer = if self.config.workers.is_empty() {
            PLANNER_PROTOCOL_FOOTER_WITH_OPERATION
        } else {
            PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES
        };
        let planner_producer_generic = RolePromptConfig {
            identity: format!("{PLANNER_PRODUCER_IDENTITY}\n{}", generic.identity),
            ..generic.clone()
        };
        let worker_producer_generic = RolePromptConfig {
            identity: format!("{WORKER_PRODUCER_IDENTITY}\n{}", generic.identity),
            ..generic.clone()
        };
        let planner_producer_base =
            render_role_prompt(&planner_producer_generic, &planner.producer, None);
        RolePolicy {
            planner_producer_system: format!("{planner_producer_base}\n{planner_protocol_footer}"),
            worker_producer_system: format!(
                "{}\n{WORK_PRODUCER_SYSTEM}",
                render_role_prompt(&worker_producer_generic, &worker.producer, None)
            ),
            planner_critic_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &planner.critic, None)
            ),
            worker_critic_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &worker.critic, None)
            ),
            planner_referee_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &planner.referee, None)
            ),
            worker_referee_system: format!(
                "{}\n{DEFAULT_SYSTEM}",
                render_role_prompt(generic, &worker.referee, None)
            ),
            planner_protocol_schema: planner_protocol_footer.to_string(),
            planner_producer_base,
            worker_role_descriptions: self
                .config
                .workers
                .iter()
                .map(|w| (w.role.clone(), w.description.clone()))
                .collect(),
            worker_role_policies: self
                .config
                .workers
                .iter()
                .map(|w| (w.role.clone(), worker_role_policy(generic, w)))
                .collect(),
        }
    }

    fn context_file_names(&self) -> Vec<String> {
        self.config.context_files.clone()
    }
}

/// Build one worker role's Producer/Critic/Referee prompts, composed the
/// same way as the shared `worker_*_system` fields in [`YamlProjectAdapter::role_policy`].
fn worker_role_policy(generic: &RolePromptConfig, worker: &WorkerRoleConfig) -> WorkerRolePolicy {
    let producer_generic = RolePromptConfig {
        identity: format!("{WORKER_PRODUCER_IDENTITY}\n{}", generic.identity),
        ..generic.clone()
    };
    WorkerRolePolicy {
        producer_system: format!(
            "{}\n{WORK_PRODUCER_SYSTEM}",
            render_role_prompt(&producer_generic, &worker.producer, None)
        ),
        critic_system: format!(
            "{}\n{DEFAULT_SYSTEM}",
            render_role_prompt(generic, &worker.critic, None)
        ),
        referee_system: format!(
            "{}\n{DEFAULT_SYSTEM}",
            render_role_prompt(generic, &worker.referee, None)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::yaml_config::{PlannerConfig, WorkerRoleConfig};
    use crate::roles::policy::PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES;

    fn prompt(instructions: &str, constraints: &str) -> RolePromptConfig {
        RolePromptConfig {
            identity: format!("{instructions} identity"),
            context: format!("{instructions} context"),
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
            plugins: vec![],
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
        // Invariant: every RolePolicy field composes the generic prompt
        // layer, then the matching config field's identity, context,
        // instructions, and constraints, as one section per label, followed
        // by the shared framework protocol constants — with no field left
        // hardcoded or swapped. Worker fields come from the first configured
        // worker role.
        let policy = adapter().role_policy();
        let generic = generic_prompt();

        assert_ordered_sections(
            &policy.planner_producer_system,
            &[
                "Identity:",
                PLANNER_PRODUCER_IDENTITY,
                &generic.identity,
                "plan it identity",
                "Context:",
                &generic.context,
                "plan it context",
                "Instructions:",
                &generic.instructions,
                "plan it",
                "Constraints:",
                &generic.constraints,
                "plan bounds",
                PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES,
            ],
        );
        assert_ordered_sections(
            &policy.worker_producer_system,
            &[
                "Identity:",
                WORKER_PRODUCER_IDENTITY,
                &generic.identity,
                "build it identity",
                "Context:",
                &generic.context,
                "build it context",
                "Instructions:",
                &generic.instructions,
                "build it",
                "Constraints:",
                &generic.constraints,
                "build bounds",
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
                    "Identity:",
                    &generic.identity,
                    &format!("{instructions} identity"),
                    "Context:",
                    &generic.context,
                    &format!("{instructions} context"),
                    "Instructions:",
                    &generic.instructions,
                    instructions,
                    "Constraints:",
                    &generic.constraints,
                    constraints,
                    DEFAULT_SYSTEM,
                ],
            );
        }
    }

    #[test]
    fn role_policy_uses_first_worker_role_when_multiple_are_configured() {
        // Invariant: with more than one worker role configured, role_policy
        // sources the shared worker_*_system fallback fields from the first
        // entry, not the last.
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
            plugins: vec![],
        });
        assert!(
            adapter
                .role_policy()
                .worker_producer_system
                .contains("build it"),
            "expected first worker role's prompt to be used"
        );
    }

    #[test]
    fn role_policy_populates_worker_role_policies_per_role() {
        // Invariant: every configured worker role gets its own entry in
        // worker_role_policies, keyed by role name, with its own
        // producer/critic/referee prompts distinct from other roles' —
        // this is what lets node dispatch pick a per-role prompt instead of
        // always falling back to the first-worker shared fields.
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
            plugins: vec![],
        });
        let policy = adapter.role_policy();

        assert_eq!(policy.worker_role_policies.len(), 2);
        let implementer = &policy.worker_role_policies["implementer"];
        assert!(implementer.producer_system.contains("build it"));
        assert!(implementer.critic_system.contains("review the work"));
        assert!(implementer.referee_system.contains("decide the work"));
        let tester = &policy.worker_role_policies["tester"];
        assert!(tester.producer_system.contains("test it"));
        assert!(tester.critic_system.contains("review the tests"));
        assert!(tester.referee_system.contains("decide the tests"));
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
                plugins: vec![],
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
