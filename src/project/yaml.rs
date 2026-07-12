//! Project adapter driven entirely by a YAML-loaded [`ProjectAdapterConfig`].

use std::collections::BTreeMap;

use super::ProjectAdapter;
use super::yaml_config::{ProjectAdapterConfig, RolePromptConfig, WorkerRoleConfig};
use crate::language::LanguageSpec;
use crate::roles::RolePolicy;
use crate::roles::policy::{
    DEFAULT_SYSTEM, PLANNER_PRODUCER_IDENTITY, WORK_PRODUCER_SYSTEM, WORKER_PRODUCER_IDENTITY,
    WorkerRolePolicy, generic_prompt, planner_protocol_schema_for, render_role_prompt,
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

    /// This adapter's first configured worker role name, if any.
    ///
    /// A single-purpose Work-only adapter (see [`ProjectAdapterConfig::workers`])
    /// defines exactly one role; this is the role a language plugin's
    /// per-role `name_target_rules` override is matched against when
    /// resolving a [`crate::config::TeamConfig`]'s name-derived targets.
    pub fn primary_worker_role(&self) -> Option<&str> {
        self.config.workers.first().map(|w| w.role.as_str())
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
        // A Plan-only adapter (no `workers` configured) never renders these
        // shared Work-node fields for any real node — a Work node always
        // needs *some* adapter to have supplied them, but this one simply
        // never drives one — so fall back to empty prompts rather than
        // panicking.
        let default_worker = WorkerRoleConfig::default();
        let worker = self.config.workers.first().unwrap_or(&default_worker);
        // Adapters that define worker roles must have the planner assign one
        // to every task, and only they may emit `kind: "work"` at all — see
        // `planner_protocol_schema_for`.
        let planner_protocol_footer = planner_protocol_schema_for(!self.config.workers.is_empty());
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
    fn assert_ordered_sections(haystack: &str, needles: &[String]) {
        let mut search_from = 0;
        for needle in needles {
            let found_at = haystack[search_from..].find(needle.as_str()).unwrap_or_else(|| {
                panic!(
                    "expected {needle:?} to appear at or after position {search_from} in:\n{haystack}"
                )
            });
            search_from += found_at + needle.len();
        }
    }

    /// The Instructions/Constraints sections render as one markdown `-`
    /// bullet per composed line, matching how `adapters/*.yaml` writes one
    /// sentence per line — split `s` the same way so needles built from raw
    /// config strings still line up with the rendered bullets.
    fn bullets(s: &str) -> Vec<String> {
        s.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| format!("- {line}"))
            .collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
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

        let mut needles = strings(&[
            "# Identity",
            PLANNER_PRODUCER_IDENTITY,
            &generic.identity,
            "plan it identity",
            "# Context",
            &generic.context,
            "plan it context",
            "# Instructions",
        ]);
        needles.extend(bullets("plan it"));
        needles.push("# Constraints".to_string());
        needles.extend(bullets(&generic.constraints));
        needles.extend(bullets("plan bounds"));
        needles.push(PLANNER_PROTOCOL_FOOTER_WITH_OPERATION_AND_ROLES.to_string());
        assert_ordered_sections(&policy.planner_producer_system, &needles);

        let mut needles = strings(&[
            "# Identity",
            WORKER_PRODUCER_IDENTITY,
            &generic.identity,
            "build it identity",
            "# Context",
            &generic.context,
            "build it context",
            "# Instructions",
        ]);
        needles.extend(bullets("build it"));
        needles.push("# Constraints".to_string());
        needles.extend(bullets(&generic.constraints));
        needles.extend(bullets("build bounds"));
        needles.push(WORK_PRODUCER_SYSTEM.to_string());
        assert_ordered_sections(&policy.worker_producer_system, &needles);

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
            let mut needles = strings(&[
                "# Identity",
                &generic.identity,
                &format!("{instructions} identity"),
                "# Context",
                &generic.context,
                &format!("{instructions} context"),
                "# Instructions",
            ]);
            needles.extend(bullets(instructions));
            needles.push("# Constraints".to_string());
            needles.extend(bullets(&generic.constraints));
            needles.extend(bullets(constraints));
            needles.push(DEFAULT_SYSTEM.to_string());
            assert_ordered_sections(system, &needles);
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
    fn role_policy_does_not_panic_with_no_workers_configured() {
        // Invariant: a Plan-only adapter (e.g. a planning-team adapter with
        // no worker roles at all) must not panic building its RolePolicy —
        // the shared Work-node fields it never actually uses simply fall
        // back to empty prompts.
        let adapter = YamlProjectAdapter::new(ProjectAdapterConfig {
            planner: planner_config(),
            workers: vec![],
            context_files: vec![],
            plugins: vec![],
        });
        let policy = adapter.role_policy();
        assert!(policy.worker_role_descriptions.is_empty());
        assert!(policy.worker_role_policies.is_empty());
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
