//! Scheduler machine phase enum.
//!
//! This module owns only `SchedulerState`, the durable checkpoint enum for the
//! scheduler state machine.  Graph and node types live in `graph.rs`.
//!
//! It does **not** own events (what the scheduler receives) or effects (what it
//! commands). Those live in `event.rs` and `effect.rs` respectively.

use serde::{Deserialize, Serialize};

use super::config::RunConfig;
use super::failure::FailureReason;
use super::graph::RunGraph;

/// The durable checkpoints of the scheduler state machine.
///
/// Each variant carries exactly the data needed to resume from that point.
/// The scheduler advances through these states as it drives the run graph
/// toward completion.
///
/// # State flow
///
/// ```text
/// Active
///   │ Start
///   ├─ invalid graph ───────────────→ Failed
///   ├─ all nodes terminal ──────────→ Complete
///   ├─ no ready nodes (deadlock) ───→ Failed
///   └─ first ready node found
///        mark Running, emit RunNode
///              ↓
///           Waiting
///              │ node completion event
///              ├─ PlanAccepted ────────→ Active   (insert children)
///              ├─ WorkAccepted ────────→ Waiting  (mark Integrating, emit IntegrateWork)
///              │    │ integration completion event
///              │    ├─ Succeeded ──────→ Active   (mark Completed)
///              │    └─ Failed ─────────→ Active | Failed  (recovery)
///              ├─ recoverable failure ─→ Active   (insert replacement)
///              └─ Terminal failure ────→ Failed   (cancel dependents)
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SchedulerState {
    /// The scheduler is ready to scan the graph and dispatch the next node.
    ///
    /// On a `Start` event the scheduler checks whether all nodes are terminal
    /// (→ `Complete`), whether the graph is deadlocked (→ `Failed`), or picks
    /// the first ready node to dispatch (→ `Waiting`).
    #[serde(rename = "Running")]
    Active {
        /// The run graph to scan and advance.
        graph: RunGraph,
        /// Run-scoped policy; `serde(default)` for checkpoint compatibility.
        #[serde(default)]
        run_config: RunConfig,
    },
    /// One node in the graph has been dispatched and the scheduler is waiting
    /// for its result. No further dispatch happens until a node completion or
    /// integration completion event arrives. The active node is derived from
    /// the single node whose status is `Running` or `Integrating`. If the node
    /// reported `WorkAccepted`, it will be in `Integrating` status and the
    /// scheduler awaits `IntegrationSucceeded` or `IntegrationFailed`.
    Waiting {
        /// The run graph with the dispatched node marked `Running` or `Integrating`.
        graph: RunGraph,
        /// Run-scoped policy; propagated from the matching `Active` state.
        #[serde(default)]
        run_config: RunConfig,
    },
    /// All nodes have reached a terminal status (`Completed`, `Failed`, or
    /// `Cancelled`) with no failures that halted the run. The graph is the
    /// complete execution record.
    Complete {
        /// The final graph with every node in a terminal status.
        graph: RunGraph,
    },
    /// The run was halted and cannot continue. The graph is preserved for
    /// post-mortem inspection.
    ///
    /// Causes include:
    /// - A `Terminal` recovery action (node reported an unrecoverable failure).
    /// - Attempt exhaustion: `Retry`, `ElevateModel`, or `Split` on a node
    ///   already at `MAX_ATTEMPTS`.
    /// - An invalid graph supplied to `Active + Start` (duplicate IDs or
    ///   missing dependency references).
    /// - An invalid node outcome: mismatched kind/outcome (e.g. `WorkAccepted`
    ///   for a `Plan` node, or `PlanAccepted` for a `Work` node).
    /// - An invalid plan output: a child request references an unknown `NodeId`.
    /// - A deadlock: no node is ready but the graph is not yet complete
    ///   (blocked dependency chain or cycle).
    Failed {
        /// The graph at the point of failure.
        graph: RunGraph,
        /// The typed cause of the failure.
        reason: FailureReason,
    },
}

impl SchedulerState {
    /// Builds the state to resume with once a single node's (or
    /// integration's) outcome has been fully resolved.
    ///
    /// With a `dispatch_cap` of more than one, resolving one in-flight node
    /// does not necessarily mean the scheduler is free to re-scan for new
    /// ready work — other dispatched nodes may still be `Running` or
    /// `Integrating`. Returns `Waiting` while any are, so their eventual
    /// return events keep being delivered to `Waiting` (see
    /// `RunGraph::resolve_in_flight`); returns `Active` only once none are,
    /// matching the historical cap-of-one behaviour exactly.
    pub(super) fn resuming(graph: RunGraph, run_config: RunConfig) -> SchedulerState {
        if graph.active_nodes().is_empty() {
            SchedulerState::Active { graph, run_config }
        } else {
            SchedulerState::Waiting { graph, run_config }
        }
    }
}
