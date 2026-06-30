//! Scheduler progress reporting.

use crate::machines::scheduler::event::{RecoveryAction, SchedulerEvent};

pub(crate) fn print_returned_progress(event: &SchedulerEvent) {
    match event {
        SchedulerEvent::NodeFailed { node_id, failure } => {
            eprintln!("[scheduler] failed {}", node_id.0);
            let recovery = match &failure.recovery {
                RecoveryAction::Retry { .. } => "Retry",
                RecoveryAction::Split { .. } => "Split",
                RecoveryAction::ElevateModel { .. } => "ElevateModel",
                RecoveryAction::Terminal { .. } => "Terminal",
            };
            eprintln!("[scheduler] recovery {recovery} {}", node_id.0);
        }
        SchedulerEvent::PlanAccepted { node_id, .. }
        | SchedulerEvent::WorkAccepted { node_id, .. } => {
            eprintln!("[scheduler] returned {}", node_id.0);
        }
        SchedulerEvent::IntegrationFailed { node_id, .. } => {
            eprintln!("[integration] failed {}", node_id.0);
        }
        SchedulerEvent::IntegrationSucceeded { node_id, .. } => {
            eprintln!("[integration] complete {}", node_id.0);
        }
        SchedulerEvent::Start => {}
    }
}

pub(crate) fn is_progress_event(event: &SchedulerEvent) -> bool {
    matches!(
        event,
        SchedulerEvent::PlanAccepted { .. }
            | SchedulerEvent::WorkAccepted { .. }
            | SchedulerEvent::NodeFailed { .. }
            | SchedulerEvent::IntegrationSucceeded { .. }
            | SchedulerEvent::IntegrationFailed { .. }
    )
}
