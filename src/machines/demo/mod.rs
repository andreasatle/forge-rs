//! Demo machine.
//!
//! This is a small example used to understand the engine/machine/runner pattern.
//! It is not part of the real Forge architecture.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;
pub mod types;

pub use machine::DemoMachine;
pub use types::{Task, TaskResult};
