//! Forge runtime — wires config into machines and drives a single run.

use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::{ForgeConfig, ProjectConfig, ProjectKind, ProjectVariant, ValidationConfig};
use crate::language::registry::language_spec;

use super::repo::load_or_create_artifact;
use crate::machines::scheduler::{
    RunConfig, RunRequest, SchedulerHandler, SchedulerMachine, SchedulerState,
    SchedulerTerminalOutput, run_scheduler_with_telemetry,
};
use crate::node_runner::{DeliberatingNodeRunner, TestTargetsFn};
use crate::project::{
    CodingProjectAdapter, CodingTddProjectAdapter, DefaultProjectAdapter, ProjectAdapter,
};
use crate::roles::RolePolicy;
use crate::runtime::checkpoint::node_counts;
use crate::runtime::provider_stack::ResolvedProviderStack;
use crate::runtime::resume::find_resumable_run;
use crate::runtime::{create_run, finalize_manifest};
use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::validation::{
    AlwaysPassValidator, CommandSpec, CommandValidator, ValidationPlan, ValidationScope,
    ValidationStage, ValidationStep, Validator,
};

/// Entry point for a single forge run driven by a [`ForgeConfig`].
pub struct ForgeRuntime;

impl ForgeRuntime {
    /// Run forge to completion using the given config.
    ///
    /// Responsibilities:
    /// 1. Load or create the bare artifact repository.
    /// 2. Create the telemetry sink.
    /// 3. Build the provider stack.
    /// 4. Drive the scheduler to completion.
    /// 5. Print a summary to stdout.
    pub fn run(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
        let artifact =
            load_or_create_artifact(&config.artifact, config.project.language.as_deref())?;

        let runs_root = PathBuf::from(&config.telemetry.directory);
        let provider_stack = ResolvedProviderStack::build(&config.provider)?;
        let run_info = create_run(
            &runs_root,
            &config.objective,
            &config.artifact.repo_path,
            &provider_stack.metadata,
        )?;
        eprintln!("[run] started {}", run_info.run_id);
        let sink: Rc<dyn TelemetrySink> =
            Rc::new(FileTelemetry::new(run_info.telemetry_dir.clone()));

        let role_policy = make_role_policy(&config.project);
        let context_file_names = make_context_file_names(&config.project);
        let required_test_targets_fn =
            make_required_test_targets_fn(&config.project, config.validation.as_ref());
        let validation_plan = make_validation_plan(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let runner = DeliberatingNodeRunner::new(provider_stack.cheap, provider_stack.strong)
            .with_cheap_max_tokens(provider_stack.cheap_tokens)
            .with_strong_max_tokens(provider_stack.strong_tokens)
            .with_role_policy(role_policy)
            .with_required_test_targets_fn(required_test_targets_fn)
            .with_context_file_names(context_file_names)
            .with_validation_plan(validation_plan);
        let validator = make_validator(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator)
            .with_checkpoint_dir(run_info.run_dir.clone());

        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: config.objective.clone(),
            },
            RunConfig {
                has_strong_tier: config.provider.strong.is_some(),
            },
        );

        let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, sink.as_ref());
        print_run_progress_result(&output);

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        let failure_reason_str: Option<String> =
            if let SchedulerTerminalOutput::Failed { reason, .. } = &output {
                Some(reason.to_string())
            } else {
                None
            };
        let (status, final_commit) = match &output {
            SchedulerTerminalOutput::Complete { .. } => (
                "succeeded",
                final_artifact.as_ref().map(|a| a.commit_sha.as_str()),
            ),
            SchedulerTerminalOutput::Failed { .. } => ("failed", None),
        };
        if let Err(e) = finalize_manifest(
            &run_info,
            status,
            final_commit,
            validation_passed,
            failure_reason_str.as_deref(),
        ) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        runtime_result_from_scheduler_terminal_output(output)
    }

    /// Resume a previously interrupted forge run.
    ///
    /// Scans `config.telemetry.directory` for a run whose `manifest.json` has
    /// `status == "running"` and loads its `graph.json` checkpoint. Exactly one
    /// such run must exist; zero or multiple produce a clear error.
    ///
    /// The restored state is normalized before re-entry: any node that was
    /// mid-execution at crash time is reset to `Pending` so the scheduler
    /// re-dispatches it. Completed work (durable in git) is preserved.
    pub fn resume(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
        let runs_root = PathBuf::from(&config.telemetry.directory);
        let (run_dir, initial_state) = find_resumable_run(&runs_root)?;
        // Re-derive has_strong_tier: it describes what provider tiers exist *now*,
        // not run history, so stale or pre-fix checkpoints don't silently inherit
        // the wrong value.
        let has_strong_tier = config.provider.strong.is_some();
        let initial_state = match initial_state {
            SchedulerState::Active { graph, .. } => SchedulerState::Active {
                graph,
                run_config: RunConfig { has_strong_tier },
            },
            SchedulerState::Waiting { graph, .. } => SchedulerState::Waiting {
                graph,
                run_config: RunConfig { has_strong_tier },
            },
            other => other,
        };
        let run_id = run_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        eprintln!("[run] resumed {run_id}");

        let artifact =
            load_or_create_artifact(&config.artifact, config.project.language.as_deref())?;
        let sink: Rc<dyn TelemetrySink> = Rc::new(FileTelemetry::new(run_dir.join("telemetry")));

        let graph = match &initial_state {
            SchedulerState::Active { graph, .. } => graph,
            _ => unreachable!("normalize_for_resume always returns Active"),
        };
        let (node_count, completed_count) = node_counts(graph);
        sink.record(TelemetryRecord::new(
            "Checkpoint",
            TelemetryEvent::CheckpointLoaded {
                node_count,
                completed_count,
            },
        ));

        let provider_stack = ResolvedProviderStack::build(&config.provider)?;

        let role_policy = make_role_policy(&config.project);
        let context_file_names = make_context_file_names(&config.project);
        let required_test_targets_fn =
            make_required_test_targets_fn(&config.project, config.validation.as_ref());
        let validation_plan = make_validation_plan(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let runner = DeliberatingNodeRunner::new(provider_stack.cheap, provider_stack.strong)
            .with_cheap_max_tokens(provider_stack.cheap_tokens)
            .with_strong_max_tokens(provider_stack.strong_tokens)
            .with_role_policy(role_policy)
            .with_required_test_targets_fn(required_test_targets_fn)
            .with_context_file_names(context_file_names)
            .with_validation_plan(validation_plan);
        let validator = make_validator(
            config.project.language.as_deref(),
            config.validation.as_ref(),
        )?;
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(validator)
            .with_checkpoint_dir(run_dir.clone());

        let run_info = crate::runtime::RunInfo {
            run_id,
            run_dir: run_dir.clone(),
            telemetry_dir: run_dir.join("telemetry"),
            started_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        };

        let (output, handler) = run_scheduler_with_telemetry(handler, initial_state, sink.as_ref());
        print_run_progress_result(&output);

        let final_artifact = handler.artifact();
        let validation_passed = handler.validation_passed();
        print_summary(&output, &config, final_artifact.as_ref(), &run_info);

        let failure_reason_str: Option<String> =
            if let SchedulerTerminalOutput::Failed { reason, .. } = &output {
                Some(reason.to_string())
            } else {
                None
            };
        let (status, final_commit) = match &output {
            SchedulerTerminalOutput::Complete { .. } => (
                "succeeded",
                final_artifact.as_ref().map(|a| a.commit_sha.as_str()),
            ),
            SchedulerTerminalOutput::Failed { .. } => ("failed", None),
        };
        if let Err(e) = finalize_manifest(
            &run_info,
            status,
            final_commit,
            validation_passed,
            failure_reason_str.as_deref(),
        ) {
            eprintln!("warning: failed to finalize manifest: {e}");
        }

        runtime_result_from_scheduler_terminal_output(output)
    }
}

fn runtime_result_from_scheduler_terminal_output(
    output: SchedulerTerminalOutput,
) -> Result<(), Box<dyn Error>> {
    match output {
        SchedulerTerminalOutput::Failed { reason, .. } => {
            Err(format!("run failed: {reason}").into())
        }
        SchedulerTerminalOutput::Complete { .. } => Ok(()),
    }
}

fn print_run_progress_result(output: &SchedulerTerminalOutput) {
    match output {
        SchedulerTerminalOutput::Complete { .. } => eprintln!("[run] complete"),
        SchedulerTerminalOutput::Failed { .. } => eprintln!("[run] failed"),
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

fn print_summary(
    output: &SchedulerTerminalOutput,
    config: &ForgeConfig,
    artifact: Option<&Artifact>,
    run_info: &crate::runtime::RunInfo,
) {
    let result_str = match output {
        SchedulerTerminalOutput::Complete { .. } => "COMPLETE",
        SchedulerTerminalOutput::Failed { .. } => "FAILED",
    };

    println!("Result      : {result_str}");
    println!("Run ID      : {}", run_info.run_id);
    println!("Artifact repo: {}", config.artifact.repo_path);

    if let Some(a) = artifact {
        let short_sha = &a.commit_sha[..a.commit_sha.len().min(7)];
        println!("Commit      : {short_sha}");
        println!("Telemetry   : {}", run_info.telemetry_dir.display());

        let view = ArtifactView {
            repo_path: a.repo_path.clone(),
            commit_sha: a.commit_sha.clone(),
        };
        if let Ok(files) = view.list_files()
            && !files.is_empty()
        {
            println!("\nGenerated files:");
            for f in &files {
                println!("  {}", f.display());
            }
        }
    } else {
        println!("Commit      : unknown");
        println!("Telemetry   : {}", run_info.telemetry_dir.display());
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
