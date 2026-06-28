//! Scheduler progress reporting.

use crate::machines::scheduler::event::{
    IntegrationOutcome, NodeOutcome, RecoveryAction, SchedulerEvent,
};

pub(crate) fn print_returned_progress(event: &SchedulerEvent) {
    match event {
        SchedulerEvent::NodeReturned { node_id, outcome } => {
            if let NodeOutcome::Failed(failure) = outcome {
                eprintln!("[scheduler] failed {}", node_id.0);
                let recovery = match &failure.recovery {
                    RecoveryAction::Retry { .. } => "Retry",
                    RecoveryAction::Split { .. } => "Split",
                    RecoveryAction::ElevateModel { .. } => "ElevateModel",
                    RecoveryAction::Terminal { .. } => "Terminal",
                };
                eprintln!("[scheduler] recovery {recovery} {}", node_id.0);
            } else {
                eprintln!("[scheduler] returned {}", node_id.0);
            }
        }
        SchedulerEvent::IntegrationReturned { node_id, outcome } => {
            if matches!(outcome, IntegrationOutcome::Failed(_)) {
                eprintln!("[integration] failed {}", node_id.0);
            } else {
                eprintln!("[integration] complete {}", node_id.0);
            }
        }
        SchedulerEvent::Start => {}
    }
}

pub(crate) fn is_progress_event(event: &SchedulerEvent) -> bool {
    matches!(
        event,
        SchedulerEvent::NodeReturned { .. } | SchedulerEvent::IntegrationReturned { .. }
    )
}
