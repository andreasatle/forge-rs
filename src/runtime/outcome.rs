//! Derives the printed, persisted, and returned outcome of a scheduler run
//! from its terminal output. Shared by [`super::run::ForgeRuntime::run`] and
//! [`super::run::ForgeRuntime::resume`], which otherwise duplicate this
//! status/commit/failure derivation.

use std::error::Error;

use crate::artifacts::{Artifact, ArtifactView};
use crate::config::ForgeConfig;
use crate::machines::scheduler::SchedulerTerminalOutput;
use crate::runtime::{RunInfo, finalize_manifest};

/// Outcome of a scheduler run, derived once from its terminal output and
/// reused for progress logging, the stdout summary, and manifest finalization.
pub struct RunOutcome {
    completed: bool,
    failure_reason: Option<String>,
}

impl RunOutcome {
    /// Derive the outcome from a scheduler's terminal output.
    pub fn from_scheduler_terminal_output(output: &SchedulerTerminalOutput) -> Self {
        match output {
            SchedulerTerminalOutput::Complete { .. } => RunOutcome {
                completed: true,
                failure_reason: None,
            },
            SchedulerTerminalOutput::Failed { reason, .. } => RunOutcome {
                completed: false,
                failure_reason: Some(reason.to_string()),
            },
        }
    }

    fn status(&self) -> &'static str {
        if self.completed {
            "succeeded"
        } else {
            "failed"
        }
    }

    /// The final artifact commit, present only when the run completed.
    pub fn final_commit<'a>(&self, artifact: Option<&'a Artifact>) -> Option<&'a str> {
        if self.completed {
            artifact.map(|a| a.commit_sha.as_str())
        } else {
            None
        }
    }

    /// Log a one-line completion/failure marker to stderr.
    pub fn print_progress(&self) {
        if self.completed {
            eprintln!("[run] complete");
        } else {
            eprintln!("[run] failed");
        }
    }

    /// Print the human-readable run summary to stdout.
    pub fn print_summary(
        &self,
        config: &ForgeConfig,
        artifact: Option<&Artifact>,
        run_info: &RunInfo,
    ) {
        let result_str = if self.completed { "COMPLETE" } else { "FAILED" };

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

    /// Merge this outcome into the run's manifest. Failure is non-fatal to
    /// the caller; the run result itself is unaffected by manifest errors.
    pub fn finalize_manifest(
        &self,
        run_info: &RunInfo,
        final_commit: Option<&str>,
        validation_passed: Option<bool>,
    ) -> Result<(), Box<dyn Error>> {
        finalize_manifest(
            run_info,
            self.status(),
            final_commit,
            validation_passed,
            self.failure_reason.as_deref(),
        )
    }

    /// Convert the outcome into the runtime's overall `Result`.
    pub fn into_result(self) -> Result<(), Box<dyn Error>> {
        match self.failure_reason {
            Some(reason) => Err(format!("run failed: {reason}").into()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
