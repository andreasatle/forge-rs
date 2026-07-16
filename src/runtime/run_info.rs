//! Run identity and directory layout for a single forge run.

use std::error::Error;
use std::path::{Path, PathBuf};

use serde::Serialize;

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

/// Effective provider endpoint and model identity for one model tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderTierMetadata {
    /// Base URL of the already-running provider server.
    pub base_url: String,
    /// Model Forge expects that server to provide.
    pub model: String,
    /// Maximum generation token budget for this tier.
    pub n_predict: usize,
    /// HTTP timeout in seconds for this tier.
    pub timeout_seconds: u64,
    /// Number of concurrent requests this tier's server can serve at once.
    /// Sizes this tier's [`crate::runtime::ResourceManager`] permit pool.
    pub parallel: usize,
    /// Whether Forge owns the provider server process for this tier.
    pub managed: bool,
    /// Managed server metadata when `managed` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_server: Option<ManagedProviderServerMetadata>,
}

/// Managed provider server metadata recorded in the run manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedProviderServerMetadata {
    /// Provider server implementation.
    pub kind: String,
    /// Executable path or command name used to start the server.
    pub command: String,
    /// Local port the server listens on.
    pub port: u16,
    /// Context size passed to the server, when configured.
    pub context_size: Option<usize>,
    /// Startup readiness timeout in seconds.
    pub startup_timeout_seconds: u64,
}

/// Effective provider metadata for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRunMetadata {
    /// Cheap/default model tier provider identity.
    pub cheap: ProviderTierMetadata,
    /// Strong model tier provider identity after fallback resolution.
    pub strong: ProviderTierMetadata,
}

/// Create a new timestamped run directory under `runs_root`.
///
/// Creates `<runs_root>/<run_id>/telemetry/`, writes `manifest.json`,
/// and updates `<runs_root>/latest` to point to the new run.
pub fn create_run(
    runs_root: &Path,
    objective: &str,
    artifact_repo: &str,
    providers: &ProviderRunMetadata,
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
        providers,
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
    providers: &ProviderRunMetadata,
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
        "provider": providers.cheap.base_url,
        "providers": providers,
    });

    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(())
}

impl RunInfo {
    /// Finalize the manifest for a completed run.
    ///
    /// Reads the existing manifest, merges outcome fields, and writes it back.
    /// If any step fails, returns an error — the caller must treat this as non-fatal.
    pub fn finalize_manifest(
        &self,
        status: &str,
        final_commit: Option<&str>,
        validation_passed: Option<bool>,
        failure_reason: Option<&str>,
    ) -> Result<(), Box<dyn Error>> {
        let manifest_path = self.run_dir.join("manifest.json");
        let content = std::fs::read_to_string(&manifest_path)?;
        let mut manifest: serde_json::Value = serde_json::from_str(&content)?;

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let completed_at = started_at_from_secs(now_secs as u64);
        let duration_seconds = (now_secs - self.started_secs).max(0.0);

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

/// Resolve `<runs_root>/latest` to the run directory it points at.
///
/// On Unix, `latest` is a symlink whose target is the run directory name; the
/// link is read explicitly (rather than followed) so the returned path is
/// `<runs_root>/<run_id>`, matching [`RunInfo::run_dir`]. On other platforms
/// [`create_run`] falls back to writing the run id as plain text into
/// `latest`, so that case is read as a file instead.
pub fn latest_run_dir(runs_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let latest = runs_root.join("latest");

    if let Ok(run_id) = std::fs::read_link(&latest) {
        return Ok(runs_root.join(run_id));
    }
    if latest.is_file() {
        let run_id = std::fs::read_to_string(&latest)?;
        return Ok(runs_root.join(run_id.trim()));
    }

    Err(format!("no runs found under {}", runs_root.display()).into())
}

#[cfg(test)]
#[path = "run_info_tests.rs"]
mod tests;
