//! Checkpoint persistence for scheduler progress.

use std::path::PathBuf;
use std::rc::Rc;

use crate::machines::scheduler::state::SchedulerState;
use crate::runtime::checkpoint::{node_counts, save_checkpoint};
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

pub(crate) struct CheckpointService {
    dir: Option<PathBuf>,
    telemetry: Rc<dyn TelemetrySink>,
}

impl CheckpointService {
    pub(crate) fn disabled(telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self {
            dir: None,
            telemetry,
        }
    }

    pub(crate) fn with_dir(self, dir: PathBuf) -> Self {
        Self {
            dir: Some(dir),
            ..self
        }
    }

    pub(crate) fn with_telemetry(self, telemetry: Rc<dyn TelemetrySink>) -> Self {
        Self { telemetry, ..self }
    }

    pub(crate) fn maybe_save(&self, state: &SchedulerState) {
        let Some(dir) = &self.dir else {
            return;
        };
        let is_active = matches!(
            state,
            SchedulerState::Active { .. } | SchedulerState::Waiting { .. }
        );
        if !is_active {
            return;
        }
        let graph = match state {
            SchedulerState::Active { graph, .. } | SchedulerState::Waiting { graph, .. } => graph,
            _ => return,
        };
        let (node_count, completed_count) = node_counts(graph);
        match save_checkpoint(dir, state) {
            Ok(()) => {
                self.telemetry.record(TelemetryRecord::new(
                    "Checkpoint",
                    TelemetryEvent::CheckpointSaved {
                        node_count,
                        completed_count,
                    },
                ));
            }
            Err(e) => {
                eprintln!("warning: failed to save checkpoint: {e}");
            }
        }
    }
}
