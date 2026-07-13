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
    /// Teams no other team's trigger names, per
    /// `ForgeConfig::terminal_teams` (computed once at config-load time from
    /// the team-trigger graph). A `ForTasks` candidate id's `depends_on`
    /// tasks must each have a completion row from every one of these teams
    /// before the candidate is eligible to spawn.
    #[serde(default)]
    pub terminal_teams: Vec<String>,
    /// Maximum number of nodes the scheduler may have `Running`/`Integrating`
    /// at once.
    ///
    /// A `Start` tick dispatches up to this many ready nodes in a single
    /// transition. `serde(default)` and the `Default` impl both pin this to
    /// `1`, matching the strictly serial dispatch behaviour every existing
    /// checkpoint and test assumes; only tests that explicitly raise it
    /// exercise concurrent in-flight tracking.
    #[serde(default = "default_dispatch_cap")]
    pub dispatch_cap: usize,
}

fn default_dispatch_cap() -> usize {
    1
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            has_strong_tier: true,
            teams: vec![],
            terminal_teams: vec![],
            dispatch_cap: default_dispatch_cap(),
        }
    }
}
