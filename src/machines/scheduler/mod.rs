//! Scheduler machine.
//!
//! Owns graph progression: selecting ready nodes, dispatching work, receiving
//! node results, and detecting completion.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;

pub use effect::SchedulerEffect;
pub use event::SchedulerEvent;
pub use machine::{SchedulerMachine, SchedulerOutput};
pub use state::{Node, NodeId, NodeStatus, RunGraph, SchedulerState};
