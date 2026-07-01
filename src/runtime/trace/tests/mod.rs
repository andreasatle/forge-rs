//! Behavior-oriented test modules for the trace reader, summary view, and
//! `run_trace` dispatch. See `default_view::tests` for the grouped default
//! view's own tests.

mod reader_tests;
mod run_trace_tests;
mod summary_tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub(super) fn temp_dir(label: &str) -> PathBuf {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "forge-trace-test-{label}-{}-{seq}",
        std::process::id()
    ))
}

/// Create `<runs_root>/<run_id>/telemetry/` and return the telemetry dir.
pub(super) fn make_run_telemetry_dir(runs_root: &std::path::Path, run_id: &str) -> PathBuf {
    let telemetry_dir = runs_root.join(run_id).join("telemetry");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    telemetry_dir
}

/// Point `<runs_root>/latest` at `run_id`, mirroring `run_info::update_latest`.
pub(super) fn set_latest(runs_root: &std::path::Path, run_id: &str) {
    let latest = runs_root.join("latest");
    let _ = std::fs::remove_file(&latest);
    #[cfg(unix)]
    std::os::unix::fs::symlink(run_id, &latest).unwrap();
    #[cfg(not(unix))]
    std::fs::write(&latest, run_id).unwrap();
}
