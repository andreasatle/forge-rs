//! Telemetry trace viewer.
//!
//! Reads the plain-text files written by
//! [`FileTelemetry`](crate::telemetry::FileTelemetry) under a run's telemetry
//! directory and prints them to stdout. The default view groups events by
//! node and attempt, showing the producer/validator/critic/referee outcome
//! of each deliberation round. `--summary` prints the old flat
//! one-line-per-event chronological view instead. `--prompts` and
//! `--failures` print full event content filtered by kind.

use std::error::Error;
use std::path::Path;

use super::run_info::latest_run_dir;

mod default_view;
mod reader;
mod summary;

#[cfg(test)]
mod tests;

/// Which trace view [`run_trace`] should print.
#[derive(Clone, Copy)]
pub enum TraceFilter {
    /// Print the node/attempt-grouped default view.
    Default,
    /// Print a one-line summary of every event, in chronological order.
    Summary,
    /// Print only `RolePromptRendered` events, with the full prompt body.
    Prompts,
    /// Print only failure-related events, with their full content.
    Failures,
}

/// Print telemetry events for one run under `runs_root` according to `filter`.
///
/// `runs_root` is the directory configured as `telemetry.directory` in the
/// forge config — it holds one subdirectory per run plus a `latest` pointer
/// to the most recently created one. When `run` is `None` the latest run is
/// used; otherwise `run` must name a run directory directly under
/// `runs_root` (the same id printed as `Run ID` in the run summary).
pub fn run_trace(
    runs_root: &str,
    run: Option<&str>,
    filter: TraceFilter,
) -> Result<(), Box<dyn Error>> {
    let runs_root = Path::new(runs_root);
    let run_dir = match run {
        Some(id) => runs_root.join(id),
        None => latest_run_dir(runs_root)?,
    };
    let paths = reader::list_telemetry_files(&run_dir)?;

    match filter {
        TraceFilter::Default => default_view::run_default_view(&run_dir, &paths)?,
        TraceFilter::Summary => summary::print_summary(&paths)?,
        TraceFilter::Prompts => summary::print_prompts(&paths)?,
        TraceFilter::Failures => summary::print_failures(&paths)?,
    }

    Ok(())
}
