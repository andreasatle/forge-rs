//! Wires an adapter name, an optional plugin name, and a [`ValidationConfig`]
//! into the pieces the scheduler runner needs: role policy, context files,
//! required test targets, validation plan, and validator.
//!
//! [`ProjectRuntimeSetup::build`] is the single entry point so `run` and
//! `resume` derive identical wiring from identical config.

use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::config::ValidationConfig;
use crate::language::registry::language_spec_for_plugin;
use crate::language::spec::LanguageSpec;
use crate::node_runner::TestTargetsFn;
use crate::project::{CodingProjectAdapter, CodingTddProjectAdapter, ProjectAdapter};
use crate::roles::RolePolicy;
use crate::validation::{
    AlwaysPassValidator, CommandSpec, CommandValidator, ValidationPlan, ValidationScope,
    ValidationStage, ValidationStep, Validator,
};

/// Project- and validation-derived wiring shared by `ForgeRuntime::run` and
/// `ForgeRuntime::resume`.
pub struct ProjectRuntimeSetup {
    pub role_policy: RolePolicy,
    pub context_file_names: Vec<String>,
    pub required_test_targets_fn: Arc<TestTargetsFn>,
    pub work_node_plan: Option<ValidationPlan>,
    pub validation_node_plan: Option<ValidationPlan>,
    pub validator: Rc<dyn Validator>,
}

impl ProjectRuntimeSetup {
    /// `adapter` names a bundled project adapter YAML file (e.g.
    /// `"coding.yaml"`); `plugin` optionally names a bundled language plugin
    /// YAML file (e.g. `"python.yaml"`). Both are validated by
    /// [`crate::config::ForgeConfig::from_file`] in the common case, but an
    /// unrecognised name is still a hard error here.
    pub fn build(
        adapter: &str,
        plugin: Option<&str>,
        validation: Option<&ValidationConfig>,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(ProjectRuntimeSetupBuilder::new(adapter, plugin, validation)?.build())
    }
}

struct ProjectRuntimeSetupBuilder<'a> {
    validation: Option<&'a ValidationConfig>,
    language_spec: Option<LanguageSpec>,
    adapter: Box<dyn ProjectAdapter>,
}

impl<'a> ProjectRuntimeSetupBuilder<'a> {
    fn new(
        adapter: &str,
        plugin: Option<&str>,
        validation: Option<&'a ValidationConfig>,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            validation,
            language_spec: Self::select_plugin(plugin)?,
            adapter: Self::select_adapter(adapter)?,
        })
    }

    fn build(&self) -> ProjectRuntimeSetup {
        ProjectRuntimeSetup {
            role_policy: self.role_policy(),
            context_file_names: self.context_file_names(),
            required_test_targets_fn: self.required_test_targets_fn(),
            work_node_plan: self.work_node_plan(),
            validation_node_plan: self.validation_node_plan(),
            validator: self.validator(),
        }
    }

    /// Selects the bundled adapter named by `adapter` (e.g. `"coding.yaml"`).
    fn select_adapter(adapter: &str) -> Result<Box<dyn ProjectAdapter>, Box<dyn Error>> {
        match adapter {
            "coding.yaml" => Ok(Box::new(CodingProjectAdapter)),
            "coding_tdd.yaml" => Ok(Box::new(CodingTddProjectAdapter)),
            other => Err(format!("unknown adapter: '{other}'").into()),
        }
    }

    /// Resolves the bundled language spec named by `plugin` (e.g. `"python.yaml"`).
    fn select_plugin(plugin: Option<&str>) -> Result<Option<LanguageSpec>, Box<dyn Error>> {
        let Some(name) = plugin else {
            return Ok(None);
        };
        language_spec_for_plugin(name)
            .map(Some)
            .ok_or_else(|| format!("unknown plugin: '{name}'").into())
    }

    fn role_policy(&self) -> RolePolicy {
        let mut policy = self.adapter.role_policy();
        if let Some(spec) = &self.language_spec {
            policy.language_guidance = Some(spec.prompt_guidance.clone());
            policy.language_constraints =
                (!spec.constraints.is_empty()).then(|| spec.constraints.clone());
        }
        policy
    }

    fn context_file_names(&self) -> Vec<String> {
        self.adapter.context_file_names()
    }

    fn required_test_targets_fn(&self) -> Arc<TestTargetsFn> {
        if !self.project_requires_tests() {
            return Arc::new(|_| vec![]);
        }
        let rules = self
            .language_spec
            .as_ref()
            .map(|spec| spec.validation.validation_targets.clone())
            .unwrap_or_default();
        Arc::new(move |targets| crate::validation::derive_validation_targets(&rules, targets))
    }

    fn project_requires_tests(&self) -> bool {
        if let Some(spec) = &self.language_spec {
            return spec.validation_includes_test_command();
        }

        // For user-supplied validation commands there is no YAML spec with an
        // explicit `runs_tests` flag, so we fall back to a heuristic: any token
        // in any command that equals "test" or ends with "test"/"tests" implies a
        // test runner is configured.
        self.validation
            .map(|config| {
                config
                    .commands
                    .iter()
                    .any(|cmd| Self::validation_command_is_test_like(cmd))
            })
            .unwrap_or(false)
    }

    fn validation_command_is_test_like(cmd: &str) -> bool {
        cmd.split_whitespace().any(|token| {
            let lower = token.to_ascii_lowercase();
            lower == "test" || lower.ends_with("test") || lower.ends_with("tests")
        })
    }

    /// Build a [`ValidationPlan`] from the language spec or explicit config.
    ///
    /// The plan is stamped onto every `Work` node at plan-expansion time.  This
    /// captures the validation contract at node-creation time so it survives
    /// checkpoint/resume unchanged, regardless of any later config edits.
    fn work_node_plan(&self) -> Option<ValidationPlan> {
        if let Some(spec) = &self.language_spec {
            Some(Self::plan_from_commands(&spec.validation.commands))
        } else {
            self.validation_config_plan()
        }
    }

    /// Build the reduced [`ValidationPlan`] stamped onto every `Validation`
    /// node at plan-expansion time.
    ///
    /// Falls back to [`Self::work_node_plan`] when the language spec declares
    /// no `validation_node_commands` (or no language plugin is configured),
    /// so `Validation` nodes are never left with a weaker contract than the
    /// project otherwise requires.
    fn validation_node_plan(&self) -> Option<ValidationPlan> {
        if let Some(spec) = &self.language_spec {
            if spec.validation.validation_node_commands.is_empty() {
                self.work_node_plan()
            } else {
                Some(Self::plan_from_commands(
                    &spec.validation.validation_node_commands,
                ))
            }
        } else {
            self.validation_config_plan()
        }
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

    fn validator(&self) -> Rc<dyn Validator> {
        if let Some(spec) = &self.language_spec {
            let timeout = Duration::from_secs(120);
            return Rc::new(CommandValidator::new(
                spec.validation.commands.clone(),
                timeout,
            ));
        }

        match self.validation {
            Some(v) if !v.commands.is_empty() => {
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
                Rc::new(CommandValidator::new(specs, timeout))
            }
            _ => Rc::new(AlwaysPassValidator),
        }
    }
}

#[cfg(test)]
#[path = "project_setup_tests.rs"]
mod tests;
