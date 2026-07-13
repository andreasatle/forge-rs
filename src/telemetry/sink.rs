//! Telemetry sink trait and built-in implementations.

use std::sync::Mutex;

use super::event::{TelemetryEvent, TelemetryRecord};

/// Receives [`TelemetryRecord`]s produced during a machine run.
///
/// Implementations decide how to handle events: discard, collect in memory,
/// or write to files. All methods take `&self` so that a sink can be passed
/// by shared reference while the runner owns the machine.
///
/// `Send + Sync`: the scheduler's concurrent dispatch spawns one thread per
/// in-flight node, each recording into the same shared sink, so every sink
/// must tolerate calls to `record` arriving from multiple threads at once.
pub trait TelemetrySink: Send + Sync {
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
    records: Mutex<Vec<TelemetryRecord>>,
}

impl VecTelemetry {
    /// Creates an empty `VecTelemetry`.
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    /// Returns a guard granting read access to the collected records so far.
    pub fn records(&self) -> std::sync::MutexGuard<'_, Vec<TelemetryRecord>> {
        self.records.lock().expect("vec telemetry mutex poisoned")
    }

    /// Consumes the sink and returns the collected events.
    pub fn into_records(self) -> Vec<TelemetryRecord> {
        self.records
            .into_inner()
            .expect("vec telemetry mutex poisoned")
    }
}

impl Default for VecTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetrySink for VecTelemetry {
    fn record(&self, record: TelemetryRecord) {
        self.records
            .lock()
            .expect("vec telemetry mutex poisoned")
            .push(record);
    }
}

/// A telemetry sink that emits human-readable progress lines to stderr and
/// delegates every record to an inner sink.
///
/// Wrap around the shared sink for a single node run so that the correct
/// `[decomposition <id>]`, `[planner <id>]`, or `[worker <id>]` label appears
/// on each progress line.
pub struct ConsoleTelemetry<'a> {
    inner: &'a dyn TelemetrySink,
    label: String,
    last_role: Mutex<Option<String>>,
}

impl<'a> ConsoleTelemetry<'a> {
    /// Create a new `ConsoleTelemetry` that prefixes every progress line with
    /// `label` (e.g. `"[decomposition a3f7c2b1]"`, `"[planner a3f7c2b1]"`, or
    /// `"[worker a3f7c2b1/tester]"`).
    pub fn new(inner: &'a dyn TelemetrySink, label: impl Into<String>) -> Self {
        Self {
            inner,
            label: label.into(),
            last_role: Mutex::new(None),
        }
    }
}

impl<'a> TelemetrySink for ConsoleTelemetry<'a> {
    fn record(&self, record: TelemetryRecord) {
        match &record.event {
            TelemetryEvent::RolePromptRendered { .. } => {
                if let Some(subsource) = &record.subsource {
                    let role = role_progress_label(subsource);
                    let mut last = self
                        .last_role
                        .lock()
                        .expect("console telemetry mutex poisoned");
                    if last.as_deref() != Some(subsource.as_str()) {
                        *last = Some(subsource.clone());
                        eprintln!("{} {role} start", self.label);
                    }
                }
                eprintln!("{} waiting for model", self.label);
            }
            TelemetryEvent::ToolRequested { tool } => {
                eprintln!("{} tool {tool}", self.label);
            }
            TelemetryEvent::ParseSucceeded { .. } => {
                if let Some(subsource) = &record.subsource {
                    let role = role_progress_label(subsource);
                    eprintln!("{} {role} complete", self.label);
                }
            }
            _ => {}
        }
        self.inner.record(record);
    }
}

fn role_progress_label(subsource: &str) -> &str {
    match subsource {
        "Producer" => "producer",
        "Critic" => "critic",
        "Referee" => "referee",
        _ => subsource,
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
}
