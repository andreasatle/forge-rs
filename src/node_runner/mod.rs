//! The NodeRunner service boundary.
//!
//! Translates a scheduler [`RunNode`](crate::machines::scheduler::SchedulerEffect) effect
//! into a typed [`NodeRunResult`], which maps back to a
//! [`NodeOutcome`](crate::machines::scheduler::NodeOutcome) via `From`.
//!
//! # Module layout
//!
//! - `types.rs` — [`NodeRunRequest`] and [`NodeRunResult`]
//! - `runner.rs` — [`NodeRunner`] trait and [`StaticNodeRunner`] fake implementation
//! - `deliberating.rs` — [`DeliberatingNodeRunner`] backed by [`DeliberationMachine`](crate::machines::deliberation::DeliberationMachine)

pub mod classify;
pub mod deliberating;
pub mod planner;
pub mod runner;
pub mod types;

pub use deliberating::DeliberatingNodeRunner;
pub use runner::{NodeRunner, StaticNodeRunner};
pub use types::{NodeRunRequest, NodeRunResult, NodeRunWorkResult};
