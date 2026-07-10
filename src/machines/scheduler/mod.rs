//! Scheduler machine.
//!
//! The scheduler drives a `RunGraph` from `Active` to either `Complete` or
//! `Failed`. It owns graph progression: selecting ready nodes, dispatching work
//! one at a time, receiving node outcomes, and routing recovery decisions.
//!
//! The transition algebra is:
//!
//! ```text
//! (SchedulerState, SchedulerEvent) -> (SchedulerState, SchedulerEffect)
//! ```
//!
//! # Module layout
//!
//! - `state.rs` — `SchedulerState`: the scheduler machine phase enum
//! - `graph.rs` — `RunGraph`, `Node`, `NodeId`, and all node/graph descriptor types
//! - `event.rs` — `SchedulerEvent`, the scheduler transition input vocabulary
//! - `effect.rs` — `SchedulerEffect` (commands emitted by transitions)
//! - `types.rs` — scheduler support payload and recovery vocabulary
//! - `machine.rs` — `SchedulerMachine` (pure transition/output), `SchedulerTerminalOutput`, `RecoverySummary`, and graph helpers
//! - `handler.rs` — `SchedulerHandler`, the impure effect executor
//! - `driver.rs` — `run_scheduler`/`run_scheduler_with_telemetry`, composing `SchedulerMachine` and `SchedulerHandler` into a drivable `Machine`
//!
//! # Output classification
//!
//! `SchedulerTerminalOutput::Complete` carries a `RecoverySummary` derived from node
//! `NodeOrigin` values, so the caller can distinguish a clean run from one that
//! required Retry, ElevateModel, or Split recovery without re-scanning the graph.
//!
//! # Key invariants
//!
//! - A node runs only when every node it depends on is `Completed`.
//! - `WorkAccepted` means work was produced; the node is not `Completed` until
//!   `IntegrationSucceeded` arrives.
//! - Failed nodes are permanent records; recovery creates replacement nodes.
//! - Retry preserves the same objective and model tier; attempt count increases.
//! - ElevateModel preserves the same objective; model tier upgrades to `Strong`.
//! - Split creates a new `Plan` node at `Strong` tier to decompose the objective.
//! - Retry, ElevateModel, and Split are attempt-limited.
//! - A `Terminal` recovery halts the entire run immediately.
//! - `NodeId` strings are opaque; graph validation must not parse them.

mod checkpoint;
mod config;
mod dispatch;
mod driver;
mod effect;
mod event;
mod failure;
mod graph;
mod handler;
#[cfg(test)]
mod handler_tests;
mod integration;
mod machine;
mod progress;
mod recovery;
mod request;
mod state;
mod triggers;
mod types;
mod validation;

pub use config::RunConfig;
pub use driver::{run_scheduler, run_scheduler_with_telemetry};
pub use effect::SchedulerEffect;
pub use event::SchedulerEvent;
pub use failure::{ExhaustedAction, FailureKind, FailureReason};
pub use graph::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, TestPlanContext,
};
pub use handler::SchedulerHandler;
pub use machine::{RecoverySummary, SchedulerMachine, SchedulerTerminalOutput};
pub use request::RunRequest;
pub use state::SchedulerState;
pub use types::{
    IntegrationFailure, IntegrationOutput, NodeFailure, NodeRequest, PlanOutput, PlannerTaskOutput,
    RecoveryAction, WorkOutput,
};
