//! Forge runtime — wires config into machines and drives a single run.

use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::ForgeConfig;

use super::repo::load_or_create_artifact;
use crate::machines::scheduler::{
    RunConfig, RunRequest, SchedulerHandler, SchedulerMachine, SchedulerState,
    SchedulerTerminalOutput, run_scheduler_with_telemetry,
};
use crate::node_runner::DeliberatingNodeRunner;
use crate::runtime::checkpoint::node_counts;
use crate::runtime::project_setup::ProjectRuntimeSetup;
use crate::runtime::provider_stack::ResolvedProviderStack;
use crate::runtime::resume::find_resumable_run;
use crate::runtime::{create_run, finalize_manifest};
use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};

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

        let setup = ProjectRuntimeSetup::build(&config.project, config.validation.as_ref())?;
        let runner = DeliberatingNodeRunner::new(provider_stack.cheap, provider_stack.strong)
            .with_cheap_max_tokens(provider_stack.cheap_tokens)
            .with_strong_max_tokens(provider_stack.strong_tokens)
            .with_role_policy(setup.role_policy)
            .with_required_test_targets_fn(setup.required_test_targets_fn)
            .with_context_file_names(setup.context_file_names)
            .with_validation_plan(setup.validation_plan);
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(setup.validator)
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

        let setup = ProjectRuntimeSetup::build(&config.project, config.validation.as_ref())?;
        let runner = DeliberatingNodeRunner::new(provider_stack.cheap, provider_stack.strong)
            .with_cheap_max_tokens(provider_stack.cheap_tokens)
            .with_strong_max_tokens(provider_stack.strong_tokens)
            .with_role_policy(setup.role_policy)
            .with_required_test_targets_fn(setup.required_test_targets_fn)
            .with_context_file_names(setup.context_file_names)
            .with_validation_plan(setup.validation_plan);
        let handler = SchedulerHandler::with_artifact(runner, artifact)
            .with_telemetry(Rc::clone(&sink))
            .with_validator(setup.validator)
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
