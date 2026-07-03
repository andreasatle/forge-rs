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
        Ok(Self {
            role_policy: make_role_policy(project),
            context_file_names: make_context_file_names(project),
            required_test_targets_fn: make_required_test_targets_fn(project, validation),
            validation_plan: make_validation_plan(project.language.as_deref(), validation)?,
            validator: make_validator(project.language.as_deref(), validation)?,
        })
    }
}

/// Selects the bundled coding adapter for `variant`.
///
/// [`ProjectKind::Coding`] can be backed by more than one prompt policy;
/// `variant` picks which one without the framework needing to know anything
/// about their contents.
fn coding_project_adapter(variant: ProjectVariant) -> Box<dyn ProjectAdapter> {
    match variant {
        ProjectVariant::Coding => Box::new(CodingProjectAdapter),
        ProjectVariant::CodingTdd => Box::new(CodingTddProjectAdapter),
    }
}

fn make_role_policy(project: &ProjectConfig) -> RolePolicy {
    let mut policy = match project.kind {
        ProjectKind::Default => DefaultProjectAdapter.role_policy(),
        ProjectKind::Coding => coding_project_adapter(project.variant).role_policy(),
    };
    if let Some(spec) = project.language.as_deref().and_then(language_spec) {
        policy.language_guidance = Some(spec.prompt_guidance);
        policy.language_constraints = (!spec.constraints.is_empty()).then_some(spec.constraints);
    }
    policy
}

fn make_context_file_names(project: &ProjectConfig) -> Vec<String> {
    match project.kind {
        ProjectKind::Default => DefaultProjectAdapter.context_file_names(),
        ProjectKind::Coding => coding_project_adapter(project.variant).context_file_names(),
    }
}

fn make_required_test_targets_fn(
    project: &ProjectConfig,
    validation: Option<&ValidationConfig>,
) -> Arc<TestTargetsFn> {
    if !project_requires_tests(project.language.as_deref(), validation) {
        return Arc::new(|_| vec![]);
    }
    let rules = project
        .language
        .as_deref()
        .and_then(language_spec)
        .map(|spec| spec.validation.validation_targets)
        .unwrap_or_default();
    Arc::new(move |targets| crate::validation::derive_validation_targets(&rules, targets))
}

fn project_requires_tests(
    language: Option<&str>,
    validation_config: Option<&ValidationConfig>,
) -> bool {
    if let Some(lang) = language
        && let Some(spec) = language_spec(lang)
    {
        return spec.validation_includes_test_command();
    }

    // For user-supplied validation commands there is no YAML spec with an
    // explicit `runs_tests` flag, so we fall back to a heuristic: any token
    // in any command that equals "test" or ends with "test"/"tests" implies a
    // test runner is configured.
    validation_config
        .map(|config| {
            config
                .commands
                .iter()
                .any(|cmd| validation_command_is_test_like(cmd))
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
fn make_validation_plan(
    language: Option<&str>,
    validation_config: Option<&ValidationConfig>,
) -> Result<Option<ValidationPlan>, Box<dyn Error>> {
    if let Some(lang) = language {
        let spec = language_spec(lang).ok_or_else(|| format!("unknown language: '{lang}'"))?;
        let steps = spec
            .validation
            .commands
            .into_iter()
            .map(|cmd| ValidationStep {
                command: std::iter::once(cmd.program).chain(cmd.args).collect(),
                when_artifacts_present: cmd.when_files_present,
                scope: cmd.scope,
                stage: ValidationStage::PreIntegration,
                must_pass: true,
            })
            .collect();
        return Ok(Some(ValidationPlan {
            steps,
            timeout_seconds: 120,
        }));
    }

    match validation_config {
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
            Ok(Some(ValidationPlan {
                steps,
                timeout_seconds,
            }))
        }
        _ => Ok(None),
    }
}

fn make_validator(
    language: Option<&str>,
    validation_config: Option<&ValidationConfig>,
) -> Result<Rc<dyn Validator>, Box<dyn Error>> {
    if let Some(lang) = language {
        let spec = language_spec(lang).ok_or_else(|| format!("unknown language: '{lang}'"))?;
        let timeout = Duration::from_secs(120);
        return Ok(Rc::new(CommandValidator::new(
            spec.validation.commands,
            timeout,
        )));
    }

    match validation_config {
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

#[cfg(test)]
#[path = "project_setup_tests.rs"]
mod tests;
