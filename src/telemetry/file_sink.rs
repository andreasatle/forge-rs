//! File-backed telemetry sink.

use std::cell::RefCell;
use std::path::PathBuf;

use super::event::TelemetryEvent;
use super::sink::TelemetrySink;

/// A sink that writes one plain-text file per event into a directory.
///
/// Files are named with a six-digit incrementing counter and the event's
/// kind slug, e.g. `000001-machine-started.txt`. This produces a
/// deterministic, alphabetically-ordered trace of a machine run.
///
/// # File format
///
/// ```text
/// kind: StateEntered
/// machine: SchedulerHandler
/// state:
/// Running {
///     graph: …
/// }
/// ```
///
/// All values come from the `TelemetryEvent` payload; no external serialiser
/// is required.
pub struct FileTelemetry {
    root: PathBuf,
    counter: RefCell<u64>,
}

impl FileTelemetry {
    /// Creates a new `FileTelemetry` that writes into `root`.
    ///
    /// The directory (and any missing ancestors) is created immediately.
    /// Returns an error if the directory cannot be created.
    pub fn new(root: PathBuf) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            counter: RefCell::new(0),
        })
    }
}

impl TelemetrySink for FileTelemetry {
    fn record(&self, event: TelemetryEvent) {
        let mut n = self.counter.borrow_mut();
        *n += 1;
        let filename = format!("{:06}-{}.txt", *n, event.kind_slug());
        let path = self.root.join(filename);
        std::fs::write(path, event.file_content()).expect("telemetry write failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-telemetry-test-{suffix}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn file_telemetry_creates_directory() {
        let dir = fresh_dir("creates");
        let _sink = FileTelemetry::new(dir.clone()).unwrap();
        assert!(dir.exists(), "telemetry root directory must be created");
    }

    #[test]
    fn file_telemetry_writes_incrementing_files() {
        let dir = fresh_dir("increments");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryEvent::MachineStarted {
            machine: "A".into(),
        });
        sink.record(TelemetryEvent::StateEntered {
            machine: "A".into(),
            state: "Running".into(),
        });
        sink.record(TelemetryEvent::EventReceived {
            machine: "A".into(),
            event: "Start".into(),
        });
        assert!(dir.join("000001-machine-started.txt").exists());
        assert!(dir.join("000002-state-entered.txt").exists());
        assert!(dir.join("000003-event-received.txt").exists());
    }

    #[test]
    fn file_telemetry_file_content_contains_kind_and_fields() {
        let dir = fresh_dir("content");
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        sink.record(TelemetryEvent::StateEntered {
            machine: "MyMachine".into(),
            state: "Idle {}".into(),
        });
        let content = std::fs::read_to_string(dir.join("000001-state-entered.txt")).unwrap();
        assert!(content.contains("kind: StateEntered"));
        assert!(content.contains("machine: MyMachine"));
        assert!(content.contains("Idle {}"));
    }
}
