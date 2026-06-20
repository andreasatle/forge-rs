//! Scheduler machine.
//!
//! Owns graph progression: selecting ready nodes, dispatching work, receiving
//! node outcomes, and routing recovery decisions.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;

pub use effect::SchedulerEffect;
pub use event::{
    NodeFailure, NodeOutcome, NodeRequest, PlanOutput, RecoveryAction, SchedulerEvent, WorkOutput,
};
pub use machine::{SchedulerMachine, SchedulerOutput};
pub use state::{ModelTier, Node, NodeId, NodeKind, NodeStatus, RunGraph, SchedulerState};
