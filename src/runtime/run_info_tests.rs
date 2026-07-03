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

fn provider_metadata() -> ProviderRunMetadata {
    ProviderRunMetadata {
        cheap: ProviderTierMetadata {
            base_url: "http://localhost:8080".to_string(),
            model: "cheap-model".to_string(),
            n_predict: 512,
            timeout_seconds: 120,
            managed: false,
            managed_server: None,
        },
        strong: ProviderTierMetadata {
            base_url: "http://localhost:8081".to_string(),
            model: "strong-model".to_string(),
            n_predict: 1024,
            timeout_seconds: 180,
            managed: false,
            managed_server: None,
        },
    }
}

#[test]
fn runtime_creates_unique_run_directories() {
    let root = temp_runs_root("unique");
    let _ = std::fs::remove_dir_all(&root);

    let r1 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    let r2 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

    assert_ne!(r1.run_id, r2.run_id, "two runs must have distinct IDs");
    assert!(r1.run_dir.exists());
    assert!(r2.run_dir.exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn previous_runs_are_preserved() {
    let root = temp_runs_root("preserved");
    let _ = std::fs::remove_dir_all(&root);

    let r1 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    let r1_dir = r1.run_dir.clone();

    let _r2 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

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

    let _r1 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    let r2 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

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

    let metadata = provider_metadata();
    let r = create_run(&root, "test objective", "repo.git", &metadata).unwrap();

    let manifest_path = r.run_dir.join("manifest.json");
    assert!(manifest_path.exists(), "manifest.json must exist");

    let content = std::fs::read_to_string(&manifest_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(v["run_id"], r.run_id.as_str());
    assert_eq!(v["telemetry_dir"], "telemetry");
    assert_eq!(v["provider"], "http://localhost:8080");

    assert_eq!(v["providers"], serde_json::to_value(&metadata).unwrap());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn telemetry_written_inside_run_directory() {
    let root = temp_runs_root("telemetry-path");
    let _ = std::fs::remove_dir_all(&root);

    let r = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

    assert!(r.telemetry_dir.starts_with(&r.run_dir));
    assert_eq!(r.telemetry_dir, r.run_dir.join("telemetry"));
    assert!(r.telemetry_dir.exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn manifest_initially_running() {
    let root = temp_runs_root("initially-running");
    let _ = std::fs::remove_dir_all(&root);

    let r = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

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

    let r = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    r.finalize_manifest("succeeded", Some("abc1234"), None, None)
        .unwrap();

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

    let r = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    r.finalize_manifest("failed", None, None, Some("integration conflict"))
        .unwrap();

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

    let r = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

    // Remove run_dir to force finalize_manifest to fail.
    std::fs::remove_dir_all(&r.run_dir).unwrap();

    let finalize_result = r.finalize_manifest("succeeded", None, None, None);
    assert!(
        finalize_result.is_err(),
        "finalize must fail when run_dir is gone"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_run_directory_deleted_when_new_run_starts() {
    let root = temp_runs_root("no-delete");
    let _ = std::fs::remove_dir_all(&root);

    let r1 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    let sentinel = r1.run_dir.join("telemetry").join("sentinel.txt");
    std::fs::write(&sentinel, "keep me").unwrap();

    let _r2 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

    assert!(
        sentinel.exists(),
        "sentinel file from first run must not be deleted"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ── latest_run_dir ────────────────────────────────────────────────────────

#[test]
fn latest_run_dir_resolves_to_newest_run() {
    let root = temp_runs_root("latest-resolves");
    let _ = std::fs::remove_dir_all(&root);

    let _r1 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();
    let r2 = create_run(&root, "obj", "repo", &provider_metadata()).unwrap();

    let resolved = latest_run_dir(&root).unwrap();
    assert_eq!(
        resolved, r2.run_dir,
        "latest_run_dir must resolve to the most recently created run"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn latest_run_dir_errors_when_no_runs_exist() {
    let root = temp_runs_root("latest-missing");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let result = latest_run_dir(&root);
    assert!(
        result.is_err(),
        "latest_run_dir must fail when no run has ever been created"
    );

    let _ = std::fs::remove_dir_all(&root);
}
