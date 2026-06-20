//! Scheduler machine.
//!
//! The scheduler drives a `RunGraph` from `Running` to either `Complete` or
//! `Failed`. It owns graph progression: selecting ready nodes, dispatching work
//! one at a time, receiving node outcomes, and routing recovery decisions.
//!
//! # Module layout
//!
//! - `state.rs` — `RunGraph`, `Node`, `SchedulerState`, and all node descriptor types
//! - `event.rs` — `SchedulerEvent`, `NodeOutcome`, `RecoveryAction`, and outcome payloads
//! - `effect.rs` — `SchedulerEffect` (commands emitted by transitions)
//! - `machine.rs` — `SchedulerMachine`, graph helpers, and the `Machine` implementation
//!
//! # Key invariants
//!
//! - A node runs only when every node it depends on is `Completed`.
//! - Failed nodes are permanent records; recovery creates replacement nodes.
//! - Retry preserves the same objective and model tier; attempt count increases.
//! - ElevateModel preserves the same objective; model tier upgrades to `Strong`.
//! - Split creates a new `Plan` node at `Strong` tier to decompose the objective.
//! - A `Terminal` recovery halts the entire run immediately.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;

pub use effect::SchedulerEffect;
pub use event::{
    IntegrationFailure, IntegrationOutcome, IntegrationOutput, NodeFailure, NodeOutcome,
    NodeRequest, PlanOutput, RecoveryAction, SchedulerEvent, WorkOutput,
};
pub use machine::{SchedulerMachine, SchedulerOutput};
pub use state::{
    ModelTier, Node, NodeId, NodeKind, NodeStatus, RunGraph, RunRequest, SchedulerState,
};
