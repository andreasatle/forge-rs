//! Scheduler progress reporting.

use crate::machines::scheduler::RecoveryAction;
use crate::machines::scheduler::event::SchedulerEvent;

pub(crate) fn print_returned_progress(event: &SchedulerEvent) {
    match event {
        SchedulerEvent::NodeFailed { node_id, failure } => {
            eprintln!("[scheduler] failed {}", node_id.short());
            let recovery = match &failure.recovery {
                RecoveryAction::Retry { .. } => "Retry",
                RecoveryAction::Split { .. } => "Split",
                RecoveryAction::ElevateModel { .. } => "ElevateModel",
                RecoveryAction::Terminal { .. } => "Terminal",
            };
            eprintln!("[scheduler] recovery {recovery} {}", node_id.short());
        }
        SchedulerEvent::PlanAccepted { node_id, .. }
        | SchedulerEvent::WorkAccepted { node_id, .. } => {
            eprintln!("[scheduler] returned {}", node_id.short());
        }
        SchedulerEvent::IntegrationFailed { node_id, .. } => {
            eprintln!("[integration] failed {}", node_id.short());
        }
        SchedulerEvent::IntegrationSucceeded { node_id, .. } => {
            eprintln!("[integration] complete {}", node_id.short());
        }
        SchedulerEvent::PlannerTasksIntegrationFailed { node_id, .. } => {
            eprintln!("[integration] failed {}", node_id.short());
        }
        SchedulerEvent::PlannerTasksIntegrated { node_id, .. } => {
            eprintln!("[integration] complete {}", node_id.short());
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
            | SchedulerEvent::PlannerTasksIntegrated { .. }
            | SchedulerEvent::PlannerTasksIntegrationFailed { .. }
    )
}
