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
    ///
    /// `language` is the engagement's single active language
    /// (`ForgeConfig::language`/`TeamConfig::language`), matching one of
    /// `adapter`'s declared plugin extensions — used to pick a single,
    /// explicit plugin for concerns with no node target files of their own
    /// to select by (repo bootstrap, API summaries, the handler-level
    /// fallback validator), instead of an arbitrary `BTreeMap` iteration
    /// order that would otherwise silently favor whichever extension sorts
    /// first.
    pub fn build(
        adapter: &Path,
        validation: Option<&ValidationConfig>,
        language: &str,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(ProjectRuntimeSetupBuilder::new(adapter, validation, language)?.build())
    }
}

/// Every validation function name a worker role selects (see
/// [`crate::project::WorkerRoleConfig::validation`]) must exist in the
/// `functions` map of *every* plugin this adapter declares, so a name that
/// only some plugins define is a hard error at config load time rather than
/// a silent panic at run time — regardless of which plugin ends up selected
/// for a given node's target files.
///
/// Shared by [`ProjectRuntimeSetupBuilder::new`] (the run's top-level
/// adapter) and `resolve_team_paths` (each team's adapter), so both fail
/// fast at config-load time rather than one of them deferring to first
/// dispatch.
pub(crate) fn validate_worker_roles(
    adapter: &YamlProjectAdapter,
    plugins: &BTreeMap<String, LanguageSpec>,
) -> Result<(), Box<dyn Error>> {
    for worker in adapter.worker_roles() {
        for name in &worker.validation {
            for (extension, spec) in plugins {
                if !spec.functions.contains_key(name) {
                    return Err(format!(
                        "worker role '{}' selects validation function '{name}', which is not defined in the plugin's functions for extension '{extension}'",
                        worker.plugin_role.clone().unwrap_or_default()
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

struct ProjectRuntimeSetupBuilder<'a> {
    validation: Option<&'a ValidationConfig>,
    language_plugins: BTreeMap<String, LanguageSpec>,
    language: String,
    adapter: Box<dyn ProjectAdapter>,
    /// Each configured worker role's own name mapped to its selected
    /// validation function names (see
    /// [`crate::project::WorkerRoleConfig::validation`]), captured here from
    /// the concrete adapter before it's boxed into `adapter: Box<dyn
    /// ProjectAdapter>`, which has no `worker_roles()` accessor.
    role_validations: BTreeMap<String, Vec<String>>,
}

impl<'a> ProjectRuntimeSetupBuilder<'a> {
    fn new(
        adapter: &Path,
        validation: Option<&'a ValidationConfig>,
        language: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let adapter = load_adapter(adapter)?;
        let language_plugins = adapter.language_plugins().clone();
        validate_worker_roles(&adapter, &language_plugins)?;
        let role_validations = adapter
            .worker_roles()
            .iter()
            .filter_map(|w| {
                w.plugin_role
                    .clone()
                    .map(|name| (name, w.validation.clone()))
            })
            .collect();
        Ok(Self {
            validation,
            language_plugins,
            language: language.to_string(),
            adapter: Box::new(adapter),
            role_validations,
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
                .active_plugin()
                .and_then(|spec| spec.api_summary.clone()),
            primary_language_init: self.active_plugin().map(|spec| spec.init.clone()),
            language_plugins: self.language_plugins.clone(),
        }
    }

    /// The engagement's configured active language plugin — used as the
    /// single, explicit choice for concerns that must pick one plugin
    /// without a node's target files to select by (repo bootstrap, API
    /// summaries, the handler-level fallback validator). `None` only when
    /// the adapter declares no plugins at all; `ForgeConfig::from_file`
    /// guarantees `language` matches a declared extension whenever any
    /// plugin is configured.
    fn active_plugin(&self) -> Option<&LanguageSpec> {
        self.language_plugins.get(&self.language)
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
    /// A role this adapter configures gets a plan built from exactly the
    /// named validation functions it selects (see
    /// [`crate::project::WorkerRoleConfig::validation`]), resolved
    /// generically against the selected plugin's `functions` map — an empty
    /// selection produces an empty plan, not a fallback. A role this adapter
    /// does not configure at all (including no role assigned) falls back to
    /// the plugin's default `validation.commands`. When no plugin matches the
    /// node's target files, falls back to the explicit `validation:` config,
    /// when present.
    fn validation_plan_for_role_fn(&self) -> Arc<ValidationPlanForRoleFn> {
        let plugins = self.language_plugins.clone();
        let fallback_plan = self.validation_config_plan();
        let role_validations = self.role_validations.clone();
        Arc::new(move |role, target_files| {
            let Some(spec) = select_plugin(&plugins, target_files) else {
                return fallback_plan.clone();
            };
            match role.and_then(|name| role_validations.get(name)) {
                Some(names) => {
                    let commands: Vec<CommandSpec> = names
                        .iter()
                        .map(|name| {
                            spec.functions.get(name).cloned().unwrap_or_else(|| {
                                panic!(
                                    "validation function '{name}' missing from plugin's \
                                     functions map; validate_worker_roles must have caught \
                                     this at config-load time"
                                )
                            })
                        })
                        .collect();
                    Some(Self::plan_from_commands(&commands))
                }
                None => Some(Self::plan_from_commands(&spec.validation.commands)),
            }
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
    /// when present, otherwise the active language plugin's commands.
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

        match self.active_plugin() {
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
