//! Wires a [`ProjectConfig`] + [`ValidationConfig`] into the pieces the
//! scheduler runner needs: role policy, context files, required test
//! targets, validation plan, and validator.
//!
//! [`ProjectRuntimeSetup::build`] is the single entry point so `run` and
//! `resume` derive identical wiring from identical config.

use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{ProjectConfig, ProjectKind, ProjectVariant, ValidationConfig};
use crate::language::registry::language_spec;
use crate::language::spec::LanguageSpec;
use crate::node_runner::TestTargetsFn;
use crate::project::{
    CodingProjectAdapter, CodingTddProjectAdapter, DefaultProjectAdapter, ProjectAdapter,
};
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
    pub validation_plan: Option<ValidationPlan>,
    pub validator: Rc<dyn Validator>,
}

impl ProjectRuntimeSetup {
    pub fn build(
        project: &ProjectConfig,
        validation: Option<&ValidationConfig>,
    ) -> Result<Self, Box<dyn Error>> {
        ProjectRuntimeSetupBuilder::new(project, validation).build()
    }
}

struct ProjectRuntimeSetupBuilder<'a> {
    project: &'a ProjectConfig,
    validation: Option<&'a ValidationConfig>,
    language_spec: Option<LanguageSpec>,
    adapter: Box<dyn ProjectAdapter>,
}

impl<'a> ProjectRuntimeSetupBuilder<'a> {
    fn new(project: &'a ProjectConfig, validation: Option<&'a ValidationConfig>) -> Self {
        Self {
            project,
            validation,
            language_spec: project.language.as_deref().and_then(language_spec),
            adapter: Self::select_adapter(project),
        }
    }

    fn build(&self) -> Result<ProjectRuntimeSetup, Box<dyn Error>> {
        Ok(ProjectRuntimeSetup {
            role_policy: self.role_policy(),
            context_file_names: self.context_file_names(),
            required_test_targets_fn: self.required_test_targets_fn(),
            validation_plan: self.validation_plan()?,
            validator: self.validator()?,
        })
    }

    /// Selects the bundled adapter for the configured project kind/variant.
    ///
    /// [`ProjectKind::Coding`] can be backed by more than one prompt policy;
    /// `variant` picks which one without the framework needing to know anything
    /// about their contents.
    fn select_adapter(project: &ProjectConfig) -> Box<dyn ProjectAdapter> {
        match project.kind {
            ProjectKind::Default => Box::new(DefaultProjectAdapter),
            ProjectKind::Coding => match project.variant {
                ProjectVariant::Coding => Box::new(CodingProjectAdapter),
                ProjectVariant::CodingTdd => Box::new(CodingTddProjectAdapter),
            },
        }
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
    /// The plan is stamped onto every Work node at plan-expansion time.  This
    /// captures the validation contract at node-creation time so it survives
    /// checkpoint/resume unchanged, regardless of any later config edits.
    fn validation_plan(&self) -> Result<Option<ValidationPlan>, Box<dyn Error>> {
        if let Some(spec) = self.language_spec()? {
            let steps = spec
                .validation
                .commands
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
            Ok(Some(ValidationPlan {
                steps,
                timeout_seconds: 120,
            }))
        } else {
            Ok(self.validation_config_plan())
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

    fn validator(&self) -> Result<Rc<dyn Validator>, Box<dyn Error>> {
        if let Some(spec) = self.language_spec()? {
            let timeout = Duration::from_secs(120);
            return Ok(Rc::new(CommandValidator::new(
                spec.validation.commands.clone(),
                timeout,
            )));
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
                Ok(Rc::new(CommandValidator::new(specs, timeout)))
            }
            _ => Ok(Rc::new(AlwaysPassValidator)),
        }
    }

    fn language_spec(&self) -> Result<Option<&LanguageSpec>, Box<dyn Error>> {
        if let Some(lang) = &self.project.language
            && self.language_spec.is_none()
        {
            return Err(format!("unknown language: '{lang}'").into());
        }
        Ok(self.language_spec.as_ref())
    }
}

#[cfg(test)]
#[path = "project_setup_tests.rs"]
mod tests;
