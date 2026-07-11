//! Resolves the adapter/northstar wiring a node run should execute under.
//!
//! A request's `adapter`/`northstar` fields are empty for the single-team
//! path, in which case the run's top-level wiring applies unchanged. For a
//! team-scoped request (non-empty fields), this loads that team's own
//! adapter and northstar text fresh, so each team's nodes run under their
//! own project adapter rather than the run's top-level one.

use crate::machines::scheduler::{FailureKind, NodeFailure, RecoveryAction};
use crate::node_runner::project_setup::ProjectRuntimeSetup;
use crate::node_runner::types::NodeRunResult;
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};

/// Loads the team's project adapter named by `adapter_path`.
///
/// Returns `Ok(None)` when `adapter_path` is empty (single-team path; the
/// caller should use the run's top-level wiring). No team `ValidationConfig`
/// exists yet, so a team's validator/validation plan always falls back to
/// its adapter's own plugin commands, same as the top-level path with no
/// explicit `validation:` config.
pub(super) fn load_team_setup(adapter_path: &str) -> Result<Option<ProjectRuntimeSetup>, String> {
    if adapter_path.trim().is_empty() {
        return Ok(None);
    }
    ProjectRuntimeSetup::build(std::path::Path::new(adapter_path), None)
        .map(Some)
        .map_err(|e| format!("failed to load team adapter '{adapter_path}': {e}"))
}

/// Reads the team's northstar text from `northstar_path`.
///
/// Returns `Ok(None)` when `northstar_path` is empty (single-team path; the
/// caller should use the run's top-level northstar).
pub(super) fn load_team_northstar(northstar_path: &str) -> Result<Option<String>, String> {
    if northstar_path.trim().is_empty() {
        return Ok(None);
    }
    std::fs::read_to_string(northstar_path)
        .map(Some)
        .map_err(|e| format!("failed to read team northstar '{northstar_path}': {e}"))
}

/// Converts a team wiring load failure into a terminal node failure.
///
/// Both `adapter` and `northstar` are resolved and validated as loadable at
/// `ForgeConfig::from_file` time, so reaching this at dispatch means the
/// filesystem changed underneath a validated config — not a recoverable
/// condition, so recovery is `Terminal`.
pub(super) fn team_wiring_failed(message: String, telemetry: &dyn TelemetrySink) -> NodeRunResult {
    telemetry.record(TelemetryRecord::new(
        "DeliberatingNodeRunner",
        TelemetryEvent::FailureClassified {
            reason: message.clone(),
            recovery: "Terminal".to_string(),
        },
    ));
    NodeRunResult::Failed(NodeFailure {
        kind: FailureKind::ProtocolFailure,
        message: message.clone(),
        recovery: RecoveryAction::Terminal { message },
    })
}
