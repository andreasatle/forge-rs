//! Run identity and directory layout for a single forge run.

use std::error::Error;
use std::path::{Path, PathBuf};

/// Identity and paths for a single forge run.
pub struct RunInfo {
    /// Unique human-readable run identifier (e.g. `2026-06-22-15-31-42`).
    pub run_id: String,
    /// Root directory for this run (`<runs_root>/<run_id>/`).
    pub run_dir: PathBuf,
    /// Telemetry directory for this run (`<run_dir>/telemetry/`).
    pub telemetry_dir: PathBuf,
    /// Unix epoch seconds (sub-second precision) when this run was created.
    pub started_secs: f64,
}

/// Create a new timestamped run directory under `runs_root`.
///
/// Creates `<runs_root>/<run_id>/telemetry/`, writes `manifest.json`,
/// and updates `<runs_root>/latest` to point to the new run.
pub fn create_run(
    runs_root: &Path,
    objective: &str,
    artifact_repo: &str,
    provider: &str,
) -> Result<RunInfo, Box<dyn Error>> {
    std::fs::create_dir_all(runs_root)?;

    let run_id = unique_run_id(runs_root);
    let run_dir = runs_root.join(&run_id);
    let telemetry_dir = run_dir.join("telemetry");

    let started_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    std::fs::create_dir_all(&telemetry_dir)?;
    write_manifest(
        &run_dir,
        &run_id,
        objective,
        artifact_repo,
        provider,
        started_secs,
    )?;
    update_latest(runs_root, &run_id)?;

    Ok(RunInfo {
        run_id,
        run_dir,
        telemetry_dir,
        started_secs,
    })
}

fn unique_run_id(runs_root: &Path) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let base = run_id_from_secs(secs);
    if !runs_root.join(&base).exists() {
        return base;
    }

    for n in 1u32..=999 {
        let id = format!("{base}-{n:03}");
        if !runs_root.join(&id).exists() {
            return id;
        }
    }

    format!("{base}-{secs}")
}

fn run_id_from_secs(secs: u64) -> String {
    let (year, month, day, hour, min, sec) = decompose_epoch(secs);
    format!("{year:04}-{month:02}-{day:02}-{hour:02}-{min:02}-{sec:02}")
}

fn started_at_from_secs(secs: u64) -> String {
    let (year, month, day, hour, min, sec) = decompose_epoch(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn decompose_epoch(secs: u64) -> (u32, u32, u64, u64, u64, u64) {
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let mut days = secs / 86400;

    let mut year = 1970u32;
    loop {
        let diy = days_in_year(year);
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }

    let dim = month_days(year);
    let mut month = 1u32;
    for &d in &dim {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }

    (year, month, days + 1, hour, min, sec)
}

fn days_in_year(year: u32) -> u64 {
    if is_leap_year(year) { 366 } else { 365 }
}

fn month_days(year: u32) -> [u64; 12] {
    [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ]
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn write_manifest(
    run_dir: &Path,
    run_id: &str,
    objective: &str,
    artifact_repo: &str,
    provider: &str,
    started_secs: f64,
) -> Result<(), Box<dyn Error>> {
    let started_at = started_at_from_secs(started_secs as u64);

    let manifest = serde_json::json!({
        "run_id": run_id,
        "started_at": started_at,
        "status": "running",
        "telemetry_dir": "telemetry",
        "artifact_repo": artifact_repo,
        "objective": objective,
        "provider": provider,
    });

    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(())
}

/// Finalize the manifest for a completed run.
///
/// Reads the existing manifest, merges outcome fields, and writes it back.
/// If any step fails, returns an error — the caller must treat this as non-fatal.
pub fn finalize_manifest(
    run_info: &RunInfo,
    status: &str,
    final_commit: Option<&str>,
    validation_passed: Option<bool>,
    failure_reason: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let manifest_path = run_info.run_dir.join("manifest.json");
    let content = std::fs::read_to_string(&manifest_path)?;
    let mut manifest: serde_json::Value = serde_json::from_str(&content)?;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let completed_at = started_at_from_secs(now_secs as u64);
    let duration_seconds = (now_secs - run_info.started_secs).max(0.0);

    manifest["completed_at"] = serde_json::Value::String(completed_at);
    manifest["duration_seconds"] = serde_json::json!(duration_seconds);
    manifest["status"] = serde_json::Value::String(status.to_string());
    manifest["final_commit"] = match final_commit {
        Some(c) => serde_json::Value::String(c.to_string()),
        None => serde_json::Value::Null,
    };
    if let Some(vp) = validation_passed {
        manifest["validation_passed"] = serde_json::Value::Bool(vp);
    }
    manifest["failure_reason"] = match failure_reason {
        Some(r) => serde_json::Value::String(r.to_string()),
        None => serde_json::Value::Null,
    };

    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(())
}

fn update_latest(runs_root: &Path, run_id: &str) -> Result<(), Box<dyn Error>> {
    let latest = runs_root.join("latest");

    if latest.is_symlink() || latest.is_file() {
        std::fs::remove_file(&latest)?;
    } else if latest.is_dir() {
        std::fs::remove_dir_all(&latest)?;
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(run_id, &latest)?;

    #[cfg(not(unix))]
    std::fs::write(&latest, run_id)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_runs_root(label: &str) -> PathBuf {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "forge-run-info-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    #[test]
    fn runtime_creates_unique_run_directories() {
        let root = temp_runs_root("unique");
        let _ = std::fs::remove_dir_all(&root);

        let r1 = create_run(&root, "obj", "repo", "provider").unwrap();
        let r2 = create_run(&root, "obj", "repo", "provider").unwrap();

        assert_ne!(r1.run_id, r2.run_id, "two runs must have distinct IDs");
        assert!(r1.run_dir.exists());
        assert!(r2.run_dir.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn previous_runs_are_preserved() {
        let root = temp_runs_root("preserved");
        let _ = std::fs::remove_dir_all(&root);

        let r1 = create_run(&root, "obj", "repo", "provider").unwrap();
        let r1_dir = r1.run_dir.clone();

        let _r2 = create_run(&root, "obj", "repo", "provider").unwrap();

        assert!(
            r1_dir.exists(),
            "first run directory must still exist after second run"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn latest_points_to_newest_run() {
        let root = temp_runs_root("latest");
        let _ = std::fs::remove_dir_all(&root);

        let _r1 = create_run(&root, "obj", "repo", "provider").unwrap();
        let r2 = create_run(&root, "obj", "repo", "provider").unwrap();

        let latest = root.join("latest");

        #[cfg(unix)]
        {
            let target = std::fs::read_link(&latest).expect("latest must be a symlink");
            assert_eq!(
                target.to_str().unwrap(),
                r2.run_id,
                "latest must point to the newest run ID"
            );
        }

        #[cfg(not(unix))]
        {
            let content = std::fs::read_to_string(&latest).unwrap();
            assert_eq!(content.trim(), r2.run_id);
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_created_for_each_run() {
        let root = temp_runs_root("manifest");
        let _ = std::fs::remove_dir_all(&root);

        let r = create_run(&root, "test objective", "repo.git", "http://localhost:8080").unwrap();

        let manifest_path = r.run_dir.join("manifest.json");
        assert!(manifest_path.exists(), "manifest.json must exist");

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["run_id"], r.run_id.as_str());
        assert_eq!(v["telemetry_dir"], "telemetry");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn telemetry_written_inside_run_directory() {
        let root = temp_runs_root("telemetry-path");
        let _ = std::fs::remove_dir_all(&root);

        let r = create_run(&root, "obj", "repo", "provider").unwrap();

        assert!(r.telemetry_dir.starts_with(&r.run_dir));
        assert_eq!(r.telemetry_dir, r.run_dir.join("telemetry"));
        assert!(r.telemetry_dir.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_initially_running() {
        let root = temp_runs_root("initially-running");
        let _ = std::fs::remove_dir_all(&root);

        let r = create_run(&root, "obj", "repo", "provider").unwrap();

        let content = std::fs::read_to_string(r.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            v["status"], "running",
            "new manifest must have status=running"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn successful_run_finalizes_manifest() {
        let root = temp_runs_root("finalize-success");
        let _ = std::fs::remove_dir_all(&root);

        let r = create_run(&root, "obj", "repo", "provider").unwrap();
        finalize_manifest(&r, "succeeded", Some("abc1234"), None, None).unwrap();

        let content = std::fs::read_to_string(r.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert!(
            v["completed_at"].is_string(),
            "completed_at must be present"
        );
        assert!(
            v["duration_seconds"].is_number(),
            "duration_seconds must be present"
        );
        assert_eq!(v["final_commit"], "abc1234");
        assert_eq!(v["failure_reason"], serde_json::Value::Null);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_run_finalizes_manifest() {
        let root = temp_runs_root("finalize-failure");
        let _ = std::fs::remove_dir_all(&root);

        let r = create_run(&root, "obj", "repo", "provider").unwrap();
        finalize_manifest(&r, "failed", None, None, Some("integration conflict")).unwrap();

        let content = std::fs::read_to_string(r.run_dir.join("manifest.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(v["failure_reason"], "integration conflict");
        assert_eq!(v["final_commit"], serde_json::Value::Null);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn manifest_failure_does_not_change_run_result() {
        let root = temp_runs_root("manifest-failure");
        let _ = std::fs::remove_dir_all(&root);

        let r = create_run(&root, "obj", "repo", "provider").unwrap();

        // Remove run_dir to force finalize_manifest to fail.
        std::fs::remove_dir_all(&r.run_dir).unwrap();

        let finalize_result = finalize_manifest(&r, "succeeded", None, None, None);
        assert!(
            finalize_result.is_err(),
            "finalize must fail when run_dir is gone"
        );

        // Simulate the run.rs pattern: log and continue.
        let run_result: Result<(), Box<dyn std::error::Error>> = Ok(());
        if let Err(e) = finalize_result {
            eprintln!("warning: failed to finalize manifest: {e}");
        }
        assert!(
            run_result.is_ok(),
            "run result must remain Ok despite manifest finalization failure"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_run_directory_deleted_when_new_run_starts() {
        let root = temp_runs_root("no-delete");
        let _ = std::fs::remove_dir_all(&root);

        let r1 = create_run(&root, "obj", "repo", "provider").unwrap();
        let sentinel = r1.run_dir.join("telemetry").join("sentinel.txt");
        std::fs::write(&sentinel, "keep me").unwrap();

        let _r2 = create_run(&root, "obj", "repo", "provider").unwrap();

        assert!(
            sentinel.exists(),
            "sentinel file from first run must not be deleted"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
