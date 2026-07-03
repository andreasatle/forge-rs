//! Node/attempt-grouped default trace view.
//!
//! Reconstructs, from the flat telemetry stream, what each scheduler node
//! did: which attempts it made, what the producer/validator/critic/referee
//! round of each attempt decided, and whether the node was ultimately
//! accepted or failed. See `parsing.rs` for how node/attempt context is
//! recovered from telemetry records that don't carry it directly, and
//! `grouping.rs` for how that context-assigned stream becomes the typed
//! summaries below.

mod grouping;
mod parsing;
mod render;

#[cfg(test)]
mod tests;

use std::error::Error;
use std::path::{Path, PathBuf};

/// What one attempt's deliberation rounds decided.
#[derive(Debug, PartialEq)]
pub(super) enum AttemptEvent {
    /// A Producer role round finished (`ParseSucceeded`). `completed` is
    /// `true` for the first round of the attempt; later rounds — triggered
    /// by validator/critic/referee rejection looping back to a fresh
    /// Producer dispatch — are reported as retries.
    Producer { completed: bool, tool_calls: u32 },
    /// Semantic validation of the Producer's accepted content.
    Validator {
        accepted: bool,
        /// First line of `ProducerValidationRetry::feedback_reason`, when rejected.
        reason: Option<String>,
    },
    /// The Critic's review of the Producer's content.
    Critic { accepted: bool, rationale: String },
    /// The Referee's review of the Producer's (and Critic's) content.
    Referee { accepted: bool, rationale: String },
    /// A role could not execute (`ProducerFailed`/`CriticFailed`/`RefereeFailed`)
    /// or the node/integration itself failed (`NodeFailed`/`IntegrationFailed`).
    RoleFailed {
        kind: String,
        phase: String,
        summary: String,
    },
    /// A project validation command failed during integration.
    ValidationFailed {
        command: Option<String>,
        exit_code: Option<i32>,
        lines: Vec<String>,
    },
}

/// Everything one scheduler attempt did, in emission order.
pub(super) struct AttemptSummary {
    pub(super) attempt: u32,
    pub(super) events: Vec<AttemptEvent>,
}

/// Whether a node's work was ultimately kept.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NodeStatus {
    /// A `Work` node's `IntegrationSucceeded`, or a `Plan` node's `PlanAccepted`.
    Accepted,
    /// A terminal `NodeFailed`/`IntegrationFailed` (`recovery: Terminal`).
    Failed,
    /// No terminal outcome was observed for this node in the trace.
    Unknown,
}

/// Everything one scheduler node did across all its attempts.
pub(super) struct NodeSummary {
    pub(super) node_id: String,
    /// `Some("Plan")`/`Some("Work")` once a `RunNode` dispatch has been seen
    /// for this node; `None` if the trace never shows one.
    pub(super) kind: Option<String>,
    pub(super) objective: Option<String>,
    pub(super) attempts: Vec<AttemptSummary>,
    pub(super) status: NodeStatus,
    /// Set alongside `status: Failed`, from the terminal failure's message.
    pub(super) last_failure: Option<String>,
}

/// Read the run's telemetry and manifest, build the node/attempt summary,
/// and print it.
pub(super) fn run_default_view(run_dir: &Path, paths: &[PathBuf]) -> Result<(), Box<dyn Error>> {
    let run_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)")
        .to_string();
    let event_count = paths.len();

    let mut parser = parsing::DefaultTraceParser::new(paths);
    let records = parser.read_records()?;
    let objective = read_objective(run_dir, &records);
    let contextualized = parser.assign_node_context(records);
    let nodes = grouping::DefaultTraceGrouper::new().group(contextualized);

    println!(
        "{}",
        render::DefaultTraceRenderer::new(&run_id, objective.as_deref(), event_count, &nodes)
            .render()
    );
    Ok(())
}

/// The run's top-level objective: `manifest.json`'s `objective` field, or —
/// when the manifest is missing or unparseable — the first `RunNode`
/// dispatch's objective from the telemetry itself.
fn read_objective(run_dir: &Path, records: &[parsing::RawRecord]) -> Option<String> {
    if let Some(objective) = read_manifest_objective(run_dir) {
        return Some(objective);
    }
    records.iter().find_map(|record| {
        if record.source != "SchedulerMachine" || record.kind != "EffectEmitted" {
            return None;
        }
        if parsing::event_variant_name(&record.body) != Some("RunNode") {
            return None;
        }
        parsing::debug_field(&record.body, "objective")
    })
}

fn read_manifest_objective(run_dir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(run_dir.join("manifest.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&content).ok()?;
    manifest
        .get("objective")
        .or_else(|| manifest.get("northstar"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}
