//! Scheduler run configuration type.

use serde::{Deserialize, Serialize};

use crate::config::TeamConfig;

/// Run-scoped policy that is constant for the lifetime of a scheduler run.
///
/// Carried inside `Active` and `Waiting` so that `SchedulerMachine::transition`
/// is fully reproducible from `(state, event)` alone — no out-of-band inputs.
///
/// `serde(default)` ensures that checkpoints written before this field existed
/// can be loaded and will default to the historical behaviour (`has_strong_tier:
/// true`, `teams: vec![]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunConfig {
    /// Whether a distinct strong-tier model is configured.
    ///
    /// When `false`, `ElevateModel` recovery cannot produce a meaningfully
    /// different result and is demoted to a `Retry` instead (or the run is
    /// failed when attempts are exhausted).
    pub has_strong_tier: bool,
    /// Configured teams, evaluated against the manifest on every
    /// `IntegrationSucceeded`/`PlannerTasksIntegrated` transition to decide
    /// which teams should have new nodes spawned for them.
    #[serde(default)]
    pub teams: Vec<TeamConfig>,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            has_strong_tier: true,
            teams: vec![],
        }
    }
}
