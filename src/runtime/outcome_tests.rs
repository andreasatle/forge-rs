use super::*;
use crate::machines::scheduler::{
    FailureReason, RecoverySummary, RunGraph, SchedulerTerminalOutput,
};
use std::path::PathBuf;

fn empty_graph() -> RunGraph {
    RunGraph { nodes: vec![] }
}

#[test]
fn failed_outcome_error_includes_provider_failure_reason() {
    let output = SchedulerTerminalOutput::Failed {
        graph: empty_graph(),
        reason: FailureReason::TerminalRecovery {
            terminal_message: "deliberation failed".to_string(),
            failure_message: "provider error (Retryable): connection refused".to_string(),
        },
    };
    let outcome = RunOutcome::from_scheduler_terminal_output(&output);
    let err = outcome
        .into_result()
        .expect_err("failed output must become an error");
    let message = err.to_string();
    assert!(message.contains("run failed"));
    assert!(message.contains("provider error (Retryable): connection refused"));
}

#[test]
fn complete_outcome_still_returns_ok() {
    let output = SchedulerTerminalOutput::Complete {
        graph: empty_graph(),
        recovery_summary: RecoverySummary {
            recovered: false,
            retry_count: 0,
            elevate_count: 0,
            split_count: 0,
        },
    };
    let outcome = RunOutcome::from_scheduler_terminal_output(&output);
    assert!(
        outcome.into_result().is_ok(),
        "Complete output must return Ok"
    );
}

#[test]
fn final_commit_present_only_when_complete() {
    let artifact = Artifact {
        repo_path: PathBuf::from("/tmp/does-not-matter"),
        branch: "main".to_string(),
        commit_sha: "abc1234".to_string(),
    };

    let complete = RunOutcome::from_scheduler_terminal_output(&SchedulerTerminalOutput::Complete {
        graph: empty_graph(),
        recovery_summary: RecoverySummary {
            recovered: false,
            retry_count: 0,
            elevate_count: 0,
            split_count: 0,
        },
    });
    assert_eq!(
        complete.final_commit(Some(&artifact)),
        Some("abc1234"),
        "a completed run must report the artifact's commit"
    );

    let failed = RunOutcome::from_scheduler_terminal_output(&SchedulerTerminalOutput::Failed {
        graph: empty_graph(),
        reason: FailureReason::TerminalRecovery {
            terminal_message: "deliberation failed".to_string(),
            failure_message: "boom".to_string(),
        },
    });
    assert_eq!(
        failed.final_commit(Some(&artifact)),
        None,
        "a failed run must not report a final commit even if an artifact exists"
    );
}
