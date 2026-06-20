//! forge-rs — a Rust implementation of Forge built around explicit state machines.
//!
//! # Architecture
//!
//! Every component follows the same contract:
//!
//! ```text
//! state + event  ->  next_state + effects
//! effect         ->  handler    ->  event
//! ```
//!
//! Business logic belongs in pure transition functions inside [`machines`].
//! Side effects — I/O, provider calls, git, tools — belong in [`handlers`].
//! The generic traversal loop lives in [`engine`].
//!
//! # Module map
//!
//! - [`engine`] — the `Machine` trait, `Transition` type, and runner loop
//! - [`machines`] — concrete state machines (scheduler, demo, …)
//! - [`handlers`] — effect executors that perform I/O and produce events
//! - [`models`] — shared domain data used across machines and handlers
//! - [`services`] — stateless data-transformation utilities

pub mod engine;
pub mod handlers;
pub mod machines;
pub mod models;
pub mod services;
