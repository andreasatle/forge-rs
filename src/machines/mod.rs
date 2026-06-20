//! Concrete state machines.
//!
//! A machine owns durable control-flow state and reacts to events by producing a
//! new state plus effects.
//!
//! Each machine should usually define:
//!
//! - `state.rs` — boxes/checkpoints
//! - `event.rs` — facts received by the machine
//! - `effect.rs` — commands emitted by the machine
//! - `machine.rs` — transition logic and machine implementation
//!
//! Not everything in Forge-rs should be a machine. Use machines for components
//! that remember where they are over time.

pub mod demo;
pub mod scheduler;
