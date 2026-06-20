//! Generic state-machine engine.
//!
//! The engine owns the traversal mechanics that are shared by every machine in
//! the system:
//!
//! - [`Machine`] — the trait every machine implements
//! - [`Transition`] — the return type of every transition function
//! - [`run_machine`] — the generic runner loop
//!
//! # What the engine does NOT own
//!
//! The engine is intentionally domain-free. It knows nothing about scheduler
//! nodes, agents, providers, tools, git, or any other Forge concept. All
//! concrete behavior belongs in [`machines`](crate::machines) and
//! [`handlers`](crate::handlers).
//!
//! # The execution protocol
//!
//! ```text
//! state + event  ──transition──►  next_state + effects
//!                                       │
//!                       ┌───────────────┘
//!                       │  effect
//!                       ▼
//!                  handle_effect
//!                       │
//!                       │  event
//!                       ▼
//!                  (next tick)
//! ```
//!
//! When a transition produces no effects, the runner re-sends `start_event` as
//! a free tick so that machines can advance through pure bookkeeping states
//! without waiting for external results.

/// The generic machine runner loop and the `Machine` trait.
pub mod runner;
/// The `Transition` return type shared by all machine transition functions.
pub mod transition;

pub use runner::{Machine, run_machine};
pub use transition::Transition;
