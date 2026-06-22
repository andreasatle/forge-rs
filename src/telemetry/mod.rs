//! File-backed telemetry for inspecting machine runs.
//!
//! Telemetry is intentionally minimal: structured events are written as plain
//! text files so that a run can be examined after the fact without any
//! external tooling.
//!
//! # Sinks
//!
//! | Sink | Behaviour |
//! |------|-----------|
//! | [`NoopTelemetry`] | Discards all events (used by [`run_machine`](crate::engine::run_machine)) |
//! | [`VecTelemetry`] | Collects events in a `Vec` — useful for testing |
//! | [`FileTelemetry`] | Writes one `.txt` file per event with incrementing names |
//!
//! # Usage
//!
//! ```rust,ignore
//! use forge_rs::telemetry::FileTelemetry;
//! use forge_rs::engine::run_machine_with_telemetry;
//! use std::path::PathBuf;
//!
//! let sink = FileTelemetry::new(PathBuf::from("/tmp/my-run")).unwrap();
//! let output = run_machine_with_telemetry(my_machine, initial_state, &sink);
//! ```

pub mod event;
pub mod file_sink;
pub mod sink;

pub use event::{TelemetryEvent, TelemetryRecord};
pub use file_sink::FileTelemetry;
pub use sink::{NoopTelemetry, TelemetrySink, VecTelemetry};
