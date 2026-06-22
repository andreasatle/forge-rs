//! Telemetry sink trait and built-in implementations.

use std::cell::RefCell;

use super::event::TelemetryEvent;

/// Receives [`TelemetryEvent`]s produced during a machine run.
///
/// Implementations decide how to handle events: discard, collect in memory,
/// or write to files. All methods take `&self` so that a sink can be passed
/// by shared reference while the runner owns the machine.
pub trait TelemetrySink {
    /// Records a single telemetry event.
    fn record(&self, event: TelemetryEvent);
}

/// A sink that silently discards every event.
///
/// Used by [`run_machine`](crate::engine::run_machine) so that machines
/// without explicit telemetry still compile against the same runner.
pub struct NoopTelemetry;

impl TelemetrySink for NoopTelemetry {
    fn record(&self, _event: TelemetryEvent) {}
}

/// A sink that collects events in memory for inspection in tests.
pub struct VecTelemetry {
    events: RefCell<Vec<TelemetryEvent>>,
}

impl VecTelemetry {
    /// Creates an empty `VecTelemetry`.
    pub fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
        }
    }

    /// Returns a borrow of the collected events.
    pub fn events(&self) -> std::cell::Ref<'_, Vec<TelemetryEvent>> {
        self.events.borrow()
    }

    /// Consumes the sink and returns the collected events.
    pub fn into_events(self) -> Vec<TelemetryEvent> {
        self.events.into_inner()
    }
}

impl Default for VecTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetrySink for VecTelemetry {
    fn record(&self, event: TelemetryEvent) {
        self.events.borrow_mut().push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_telemetry_collects_events() {
        let sink = VecTelemetry::new();
        sink.record(TelemetryEvent::MachineStarted {
            machine: "Test".into(),
        });
        sink.record(TelemetryEvent::StateEntered {
            machine: "Test".into(),
            state: "Idle".into(),
        });
        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], TelemetryEvent::MachineStarted { .. }));
        assert!(matches!(events[1], TelemetryEvent::StateEntered { .. }));
    }

    #[test]
    fn noop_telemetry_discards_events() {
        let sink = NoopTelemetry;
        sink.record(TelemetryEvent::MachineStarted {
            machine: "Test".into(),
        });
        sink.record(TelemetryEvent::Failure {
            component: "X".into(),
            reason: "gone".into(),
        });
        // no assertion — verify it compiles and does not panic
    }
}
