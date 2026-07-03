//! The run's top-level objective: sourced from `manifest.json`, with a
//! telemetry-derived fallback when the manifest is missing or unparseable.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::parsing::DefaultTraceParser;
use super::super::read_objective;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temp_run_dir(label: &str) -> PathBuf {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "forge-default-view-manifest-{label}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_node_record(objective: &str) -> super::super::parsing::RawRecord {
    DefaultTraceParser::parse_record(&format!(
        "source: SchedulerMachine\nnode_id: root\nattempt: 0\nkind: EffectEmitted\nmachine: SchedulerMachine\neffect:\nRunNode {{\n    kind: Work,\n    objective: \"{objective}\",\n}}\n"
    ))
    .unwrap()
}

#[test]
fn objective_field_is_read_from_manifest() {
    let dir = temp_run_dir("objective-field");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"objective": "manifest objective"}"#,
    )
    .unwrap();

    let objective = read_objective(&dir, &[]);
    assert_eq!(objective.as_deref(), Some("manifest objective"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn northstar_field_is_used_when_objective_field_is_absent() {
    let dir = temp_run_dir("northstar-fallback");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"northstar": "legacy field name"}"#,
    )
    .unwrap();

    let objective = read_objective(&dir, &[]);
    assert_eq!(objective.as_deref(), Some("legacy field name"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_manifest_falls_back_to_first_run_node_objective() {
    let dir = temp_run_dir("missing-manifest");
    let records = vec![run_node_record("telemetry-derived objective")];

    let objective = read_objective(&dir, &records);
    assert_eq!(objective.as_deref(), Some("telemetry-derived objective"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn manifest_present_takes_priority_over_telemetry_fallback() {
    let dir = temp_run_dir("manifest-priority");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"objective": "from manifest"}"#,
    )
    .unwrap();
    let records = vec![run_node_record("from telemetry")];

    let objective = read_objective(&dir, &records);
    assert_eq!(objective.as_deref(), Some("from manifest"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_manifest_and_no_run_node_yields_none() {
    let dir = temp_run_dir("no-source");
    let objective = read_objective(&dir, &[]);
    assert_eq!(objective, None);
    let _ = std::fs::remove_dir_all(&dir);
}
