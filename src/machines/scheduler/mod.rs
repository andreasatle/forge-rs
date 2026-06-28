//! Scheduler machine.
//!
//! The scheduler drives a `RunGraph` from `Running` to either `Complete` or
//! `Failed`. It owns graph progression: selecting ready nodes, dispatching work
//! one at a time, receiving node outcomes, and routing recovery decisions.
//!
//! # Module layout
//!
//! - `state.rs` — `RunGraph`, `Node`, `NodeOrigin`, `SchedulerState`, and all node descriptor types
//! - `event.rs` — `SchedulerEvent`, `NodeOutcome`, `RecoveryAction`, and outcome payloads
//! - `effect.rs` — `SchedulerEffect` (commands emitted by transitions)
//! - `machine.rs` — `SchedulerMachine`, `SchedulerOutput`, `RecoverySummary`, graph helpers, and the `Machine` implementation
//!
//! # Output classification
//!
//! `SchedulerOutput::Complete` carries a `RecoverySummary` derived from node
//! `NodeOrigin` values, so the caller can distinguish a clean run from one that
//! required Retry, ElevateModel, or Split recovery without re-scanning the graph.
//!
//! # Key invariants
//!
//! - A node runs only when every node it depends on is `Completed`.
//! - `WorkAccepted` means work was produced; the node is not `Completed` until
//!   `IntegrationReturned(Succeeded)` arrives.
//! - Failed nodes are permanent records; recovery creates replacement nodes.
//! - Retry preserves the same objective and model tier; attempt count increases.
//! - ElevateModel preserves the same objective; model tier upgrades to `Strong`.
//! - Split creates a new `Plan` node at `Strong` tier to decompose the objective.
//! - Retry, ElevateModel, and Split are attempt-limited.
//! - A `Terminal` recovery halts the entire run immediately.
//! - `NodeId` strings are opaque; graph validation must not parse them.
//! - `RunGraph::next_id` is only an internal generator cursor.

pub mod effect;
pub mod event;
mod graph;
pub mod handler;
pub mod machine;
mod recovery;
pub mod state;

pub use effect::SchedulerEffect;
pub use event::{
    FailureKind, IntegrationFailure, IntegrationOutcome, IntegrationOutput, NodeFailure,
    NodeOutcome, NodeRequest, PlanOutput, RecoveryAction, SchedulerEvent, WorkOutput,
};
pub use handler::SchedulerHandler;
pub use machine::{RecoverySummary, SchedulerMachine, SchedulerOutput};
pub use state::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest, SchedulerState,
};
