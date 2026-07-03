//! Forge runtime — wires config into machines and drives a single run.

use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::process::Command;

use crate::config::ForgeConfig;

use crate::machines::scheduler::{RunConfig, RunRequest, SchedulerMachine, SchedulerState};
use crate::runtime::create_run;
use crate::runtime::provider_stack::ResolvedProviderStack;
use crate::runtime::resume::find_resumable_run;
use crate::runtime::session::RunSession;
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

        let initial_state = SchedulerMachine::initial_state(
            RunRequest {
                objective: config.objective.clone(),
            },
            RunConfig {
                has_strong_tier: config.provider.strong.is_some(),
            },
        );

        RunSession::new(config, run_info, sink, provider_stack).drive(initial_state)
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

        let sink: Rc<dyn TelemetrySink> = Rc::new(FileTelemetry::new(run_dir.join("telemetry")));

        let graph = match &initial_state {
            SchedulerState::Active { graph, .. } => graph,
            _ => unreachable!("normalize_for_resume always returns Active"),
        };
        let (node_count, completed_count) = graph.node_counts();
        sink.record(TelemetryRecord::new(
            "Checkpoint",
            TelemetryEvent::CheckpointLoaded {
                node_count,
                completed_count,
            },
        ));

        let provider_stack = ResolvedProviderStack::build(&config.provider)?;

        let run_info = crate::runtime::RunInfo {
            run_id,
            run_dir: run_dir.clone(),
            telemetry_dir: run_dir.join("telemetry"),
            started_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        };

        RunSession::new(config, run_info, sink, provider_stack).drive(initial_state)
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
