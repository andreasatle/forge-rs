//! `run_trace` run-resolution behavior: latest-pointer resolution, explicit
//! `--run` selection, and error handling for missing runs.

use crate::runtime::trace::{TraceFilter, run_trace};
use crate::telemetry::{FileTelemetry, TelemetryEvent, TelemetryRecord, TelemetrySink};

use super::{make_run_telemetry_dir, set_latest, temp_dir};

#[test]
fn missing_runs_root_returns_error() {
    let dir = temp_dir("missing-root");
    let result = run_trace(dir.to_str().unwrap(), None, TraceFilter::Summary);
    assert!(
        result.is_err(),
        "trace over a nonexistent runs root must return an error"
    );
}

#[test]
fn missing_latest_pointer_returns_error() {
    let dir = temp_dir("missing-latest");
    std::fs::create_dir_all(&dir).unwrap();
    let result = run_trace(dir.to_str().unwrap(), None, TraceFilter::Summary);
    assert!(
        result.is_err(),
        "trace with no `latest` pointer and no runs must return an error"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_run_resolves_to_latest() {
    let root = temp_dir("default-latest");
    let telemetry_dir = make_run_telemetry_dir(&root, "run-a");
    let sink = FileTelemetry::new(telemetry_dir);
    sink.record(TelemetryRecord::new(
        "SchedulerMachine",
        TelemetryEvent::MachineStarted {
            machine: "SchedulerHandler".into(),
        },
    ));
    set_latest(&root, "run-a");

    let result = run_trace(root.to_str().unwrap(), None, TraceFilter::Summary);
    assert!(
        result.is_ok(),
        "trace with no --run must resolve the `latest` pointer and succeed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn explicit_run_bypasses_latest() {
    let root = temp_dir("explicit-run");
    let telemetry_dir = make_run_telemetry_dir(&root, "run-b");
    let sink = FileTelemetry::new(telemetry_dir);
    sink.record(TelemetryRecord::new(
        "SchedulerMachine",
        TelemetryEvent::MachineStarted {
            machine: "SchedulerHandler".into(),
        },
    ));
    // No `latest` pointer is set: an explicit --run must not need it.

    let result = run_trace(root.to_str().unwrap(), Some("run-b"), TraceFilter::Summary);
    assert!(
        result.is_ok(),
        "trace with an explicit --run must not require a `latest` pointer"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unknown_explicit_run_returns_error() {
    let root = temp_dir("unknown-run");
    std::fs::create_dir_all(&root).unwrap();

    let result = run_trace(
        root.to_str().unwrap(),
        Some("no-such-run"),
        TraceFilter::Summary,
    );
    assert!(
        result.is_err(),
        "trace with a --run that does not exist under runs_root must fail"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn non_txt_files_in_telemetry_directory_are_ignored() {
    let root = temp_dir("ignore-non-txt");
    let telemetry_dir = make_run_telemetry_dir(&root, "run-a");
    let sink = FileTelemetry::new(telemetry_dir.clone());
    sink.record(TelemetryRecord::new(
        "SchedulerMachine",
        TelemetryEvent::MachineStarted {
            machine: "SchedulerHandler".into(),
        },
    ));
    std::fs::write(telemetry_dir.join("notes.md"), "not telemetry").unwrap();
    set_latest(&root, "run-a");

    // Must not error even though a non-.txt file with unrelated content exists.
    let result = run_trace(root.to_str().unwrap(), None, TraceFilter::Summary);
    assert!(result.is_ok());

    let _ = std::fs::remove_dir_all(&root);
}
