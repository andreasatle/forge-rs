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
//! The generic traversal loop lives in [`engine`].
//!
//! # Module map
//!
//! - [`engine`] — the `Machine` trait, `Transition` type, and runner loop
//! - [`machines`] — concrete state machines (scheduler, demo, …)
//! - [`providers`] — `ProviderClient` trait and typed request/response/error types
//! - [`project`] — [`ProjectAdapter`](project::ProjectAdapter) seam for project-specific config
//! - [`services`] — stateless data-transformation utilities

#![deny(missing_docs)]
pub mod artifacts;
pub mod config;
pub mod engine;
pub mod language;
pub mod machines;
pub mod node_runner;
pub mod project;
pub mod providers;
pub mod roles;
pub mod runtime;
pub mod services;
pub mod telemetry;
pub mod tools;
pub mod validation;
