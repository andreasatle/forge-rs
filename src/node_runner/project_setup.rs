//! Wires an adapter (and the language plugins it declares) plus a
//! [`ValidationConfig`] into the pieces a node runner needs: role policy,
//! context files, required test targets, validation plan, and validator.
//!
//! [`ProjectRuntimeSetup::build`] is the single entry point so `run`,
//! `resume`, and per-team dispatch inside [`DeliberatingNodeRunner`](super::DeliberatingNodeRunner)
//! derive identical wiring from identical config.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::config::ValidationConfig;
use crate::language::select_plugin;
use crate::language::spec::{LanguageInitSpec, LanguageSpec};
use crate::project::{ProjectAdapter, YamlProjectAdapter, load_adapter};
use crate::roles::RolePolicy;
use crate::validation::{
    AlwaysPassValidator, CommandSpec, CommandValidator, ValidationPlan, ValidationScope,
    ValidationStage, ValidationStep, Validator,
};

use super::{TestTargetsFn, ValidationPlanForRoleFn};

/// Project- and validation-derived wiring shared by `ForgeRuntime::run` and
/// `ForgeRuntime::resume`.
pub struct ProjectRuntimeSetup {
    pub role_policy: RolePolicy,
    pub context_file_names: Vec<String>,
    pub required_test_targets_fn: Arc<TestTargetsFn>,
    pub validation_plan_for_role_fn: Arc<ValidationPlanForRoleFn>,
    pub validator: Arc<dyn Validator>,
    pub api_summary_command: Option<CommandSpec>,
    /// Init commands for the adapter's first declared language plugin
    /// (ordered by extension), used to bootstrap a brand-new artifact
    /// repository. `None` when the adapter declares no language plugins.
    pub primary_language_init: Option<LanguageInitSpec>,
    /// This adapter's declared language plugins, keyed by extension —
    /// forwarded to the node runner so it can select the plugin whose
    /// prompt sections apply to each node's own target files, rather than
    /// baking every plugin's guidance into every prompt regardless of
    /// language.
    pub language_plugins: BTreeMap<String, LanguageSpec>,
}

impl ProjectRuntimeSetup {
    /// `adapter` is a path to a project adapter YAML file, which declares
    /// its own language plugins (`plugins:`). Validated by
    /// [`crate::config::ForgeConfig::from_file`] in the common case, but a
    /// missing/invalid adapter or plugin is still a hard error here.
    pub fn build(
        adapter: &Path,
        validation: Option<&ValidationConfig>,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(ProjectRuntimeSetupBuilder::new(adapter, validation)?.build())
    }
}

/// Every worker role the adapter defines must have a matching entry in each
/// configured plugin's `plugin_roles` list, so a missing per-role validation
/// override is a hard error at config load time rather than a silent
/// fallback to that plugin's default validation at run time — regardless of
/// which plugin ends up selected for a given node.
///
/// Also: whenever the adapter declares any plugins at all, every worker
/// entry must actually name a `plugin_role` — there is no other way to
/// select that role's per-plugin validation override. An adapter with no
/// plugins declared has nothing to match against, so its worker entries may
/// omit `plugin_role` entirely (see [`crate::project::WorkerRoleConfig::plugin_role`]).
///
/// Shared by [`ProjectRuntimeSetupBuilder::new`] (the run's top-level
/// adapter) and `resolve_team_paths` (each team's adapter), so both fail
/// fast at config-load time rather than one of them deferring to first
/// dispatch.
pub(crate) fn validate_worker_roles(
    adapter: &YamlProjectAdapter,
    plugins: &BTreeMap<String, LanguageSpec>,
) -> Result<(), Box<dyn Error>> {
    if !plugins.is_empty() {
        for worker in adapter.worker_roles() {
            if worker.plugin_role.is_none() {
                return Err(format!(
                    "worker role '{}' has no plugin_role, but this adapter declares plugins; plugin_role is required for every worker role",
                    worker.description
                )
                .into());
            }
        }
    }

    let worker_roles = adapter.role_policy().worker_role_descriptions;
    for (extension, spec) in plugins {
        let plugin_roles: std::collections::HashSet<&str> = spec
            .plugin_roles
            .iter()
            .map(|role| role.plugin_role.as_str())
            .collect();
        for (role, _) in &worker_roles {
            if !plugin_roles.contains(role.as_str()) {
                return Err(format!(
                    "adapter plugin_role '{role}' is not defined in the plugin's plugin_roles for extension '{extension}'"
                )
                .into());
            }
        }
    }
    Ok(())
}

struct ProjectRuntimeSetupBuilder<'a> {
    validation: Option<&'a ValidationConfig>,
    language_plugins: BTreeMap<String, LanguageSpec>,
    adapter: Box<dyn ProjectAdapter>,
}

impl<'a> ProjectRuntimeSetupBuilder<'a> {
    fn new(
        adapter: &Path,
        validation: Option<&'a ValidationConfig>,
    ) -> Result<Self, Box<dyn Error>> {
        let adapter = load_adapter(adapter)?;
        let language_plugins = adapter.language_plugins().clone();
        validate_worker_roles(&adapter, &language_plugins)?;
        Ok(Self {
            validation,
            language_plugins,
            adapter: Box::new(adapter),
        })
    }

    fn build(&self) -> ProjectRuntimeSetup {
        ProjectRuntimeSetup {
            role_policy: self.role_policy(),
            context_file_names: self.context_file_names(),
            required_test_targets_fn: self.required_test_targets_fn(),
            validation_plan_for_role_fn: self.validation_plan_for_role_fn(),
            validator: self.validator(),
            api_summary_command: self
                .first_plugin()
                .and_then(|spec| spec.api_summary.clone()),
            primary_language_init: self.first_plugin().map(|spec| spec.init.clone()),
            language_plugins: self.language_plugins.clone(),
        }
    }

    /// The adapter's first declared language plugin in extension order —
    /// used as a deterministic fallback for concerns that must pick a single
    /// plugin without a node's target files to select by (repo bootstrap,
    /// API summaries, the handler-level fallback validator).
    fn first_plugin(&self) -> Option<&LanguageSpec> {
        self.language_plugins.values().next()
    }

    fn role_policy(&self) -> RolePolicy {
        self.adapter.role_policy()
    }

    fn context_file_names(&self) -> Vec<String> {
        self.adapter.context_file_names()
    }

    /// Builds the adapter-provided test-target derivation function: for a
    /// node's target files, selects the matching plugin by extension and, if
    /// that plugin requires tests, derives the validation targets its rules
    /// imply.
    fn required_test_targets_fn(&self) -> Arc<TestTargetsFn> {
        let plugins = self.language_plugins.clone();
        Arc::new(move |targets| crate::language::required_validation_targets(&plugins, targets))
    }

    /// Builds the per-role validation plan lookup stamped onto every `Work`
    /// node at plan-expansion time, keyed by the node's target files (to
    /// select the matching plugin) and its assigned worker role.
    ///
    /// A role present in the selected plugin's `plugin_roles` list gets that
    /// role's own `validation.commands`; every other role (including no role
    /// at all) falls back to the plugin's default `validation.commands`. When
    /// no plugin matches the node's target files, falls back to the explicit
    /// `validation:` config, when present.
    fn validation_plan_for_role_fn(&self) -> Arc<ValidationPlanForRoleFn> {
        let plugins = self.language_plugins.clone();
        let fallback_plan = self.validation_config_plan();
        Arc::new(move |role, target_files| {
            let Some(spec) = select_plugin(&plugins, target_files) else {
                return fallback_plan.clone();
            };
            let commands = role
                .and_then(|name| spec.plugin_roles.iter().find(|r| r.plugin_role == name))
                .map(|r| &r.validation.commands)
                .unwrap_or(&spec.validation.commands);
            Some(Self::plan_from_commands(commands))
        })
    }

    /// Convert a slice of language-spec [`CommandSpec`]s into a [`ValidationPlan`].
    fn plan_from_commands(commands: &[CommandSpec]) -> ValidationPlan {
        let steps = commands
            .iter()
            .cloned()
            .map(|cmd| ValidationStep {
                command: std::iter::once(cmd.program).chain(cmd.args).collect(),
                when_artifacts_present: cmd.when_files_present,
                scope: cmd.scope,
                stage: ValidationStage::PreIntegration,
                must_pass: true,
            })
            .collect();
        ValidationPlan {
            steps,
            timeout_seconds: 120,
        }
    }

    fn validation_config_plan(&self) -> Option<ValidationPlan> {
        match self.validation {
            Some(v) if !v.commands.is_empty() => {
                let timeout_seconds = v.timeout_seconds.unwrap_or(120);
                let steps = v
                    .commands
                    .iter()
                    .map(|cmd| ValidationStep {
                        command: vec!["sh".to_string(), "-c".to_string(), cmd.clone()],
                        when_artifacts_present: vec![],
                        scope: ValidationScope::Workspace,
                        stage: ValidationStage::PreIntegration,
                        must_pass: true,
                    })
                    .collect();
                Some(ValidationPlan {
                    steps,
                    timeout_seconds,
                })
            }
            _ => None,
        }
    }

    /// Handler-level fallback validator, used only for nodes that carry no
    /// per-node `validation_plan` (e.g. no plugin matched their target files
    /// at plan-expansion time). Prefers the explicit `validation:` config
    /// when present, otherwise the first configured plugin's commands.
    fn validator(&self) -> Arc<dyn Validator> {
        if let Some(v) = self.validation
            && !v.commands.is_empty()
        {
            let timeout = Duration::from_secs(v.timeout_seconds.unwrap_or(120));
            let specs = v
                .commands
                .iter()
                .map(|cmd| CommandSpec {
                    program: "sh".to_string(),
                    args: vec!["-c".to_string(), cmd.clone()],
                    when_files_present: vec![],
                    scope: ValidationScope::Workspace,
                })
                .collect();
            return Arc::new(CommandValidator::new(specs, timeout));
        }

        match self.first_plugin() {
            Some(spec) => {
                let timeout = Duration::from_secs(120);
                Arc::new(CommandValidator::new(
                    spec.validation.commands.clone(),
                    timeout,
                ))
            }
            None => Arc::new(AlwaysPassValidator),
        }
    }
}

#[cfg(test)]
#[path = "project_setup_tests.rs"]
mod tests;
