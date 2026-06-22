//! Telemetry sink trait and built-in implementations.

use std::cell::RefCell;

use super::event::TelemetryRecord;

/// Receives [`TelemetryRecord`]s produced during a machine run.
///
/// Implementations decide how to handle events: discard, collect in memory,
/// or write to files. All methods take `&self` so that a sink can be passed
/// by shared reference while the runner owns the machine.
pub trait TelemetrySink {
    /// Records a single sourced telemetry event.
    fn record(&self, record: TelemetryRecord);
}

/// A sink that silently discards every event.
///
/// Used by [`run_machine`](crate::engine::run_machine) so that machines
/// without explicit telemetry still compile against the same runner.
pub struct NoopTelemetry;

impl TelemetrySink for NoopTelemetry {
    fn record(&self, _record: TelemetryRecord) {}
}

/// A sink that collects events in memory for inspection in tests.
pub struct VecTelemetry {
    records: RefCell<Vec<TelemetryRecord>>,
}

impl VecTelemetry {
    /// Creates an empty `VecTelemetry`.
    pub fn new() -> Self {
        Self {
            records: RefCell::new(Vec::new()),
        }
    }

    /// Returns a borrow of the collected records.
    pub fn records(&self) -> std::cell::Ref<'_, Vec<TelemetryRecord>> {
        self.records.borrow()
    }

    /// Consumes the sink and returns the collected events.
    pub fn into_records(self) -> Vec<TelemetryRecord> {
        self.records.into_inner()
    }
}

impl Default for VecTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetrySink for VecTelemetry {
    fn record(&self, record: TelemetryRecord) {
        self.records.borrow_mut().push(record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::TelemetryEvent;

    #[test]
    fn vec_telemetry_collects_events() {
        let sink = VecTelemetry::new();
        sink.record(TelemetryRecord::new(
            "Test",
            TelemetryEvent::MachineStarted {
                machine: "Test".into(),
            },
        ));
        let records = sink.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, "Test");
        assert!(matches!(
            records[0].event,
            TelemetryEvent::MachineStarted { .. }
        ));
    }

    #[test]
    fn noop_telemetry_discards_events() {
        let sink = NoopTelemetry;
        sink.record(TelemetryRecord::new(
            "Test",
            TelemetryEvent::MachineStarted {
                machine: "Test".into(),
            },
        ));
        // no assertion — verify it compiles and does not panic
    }
}
