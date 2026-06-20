//! Generic state-machine engine.
//!
//! The engine owns traversal mechanics shared by all machines:
//!
//! - the `Machine` trait
//! - the `Transition` return type
//! - the generic runner loop
//!
//! The engine does not know about scheduler nodes, agents, providers, tools, or
//! git. Concrete behavior belongs in machines and handlers.

pub mod runner;
pub mod transition;

pub use runner::{Machine, run_machine};
pub use transition::Transition;
