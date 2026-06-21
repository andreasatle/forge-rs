//! DeliberationMachine — Producer / Critic / Referee revision loop.
//!
//! This machine owns the multi-role deliberation pipeline. A single
//! `DeliberationRequest` enters; a `DeliberationOutput` (or failure) exits.
//!
//! Deliberation runs producer output through critic review before completion:
//! - `Ready + Start` dispatches `RunRole(Producer, input=None)` → `Waiting(Producer)`.
//! - `Waiting(Producer) + RoleReturned(Producer, Accepted { content })`
//!   → `Waiting(Critic)` + `RunRole(Critic, input=Some(content))`.
//! - `Waiting(Producer) + RoleReturned(Producer, Rejected)` → `Failed`.
//! - `Waiting(Critic) + RoleReturned(Critic, Accepted)` → `Complete` with
//!   **producer** content (Critic acceptance approves producer output; it does
//!   not replace it).
//! - `Waiting(Critic) + RoleReturned(Critic, Rejected)` → `Failed`.
//! - `Waiting(Critic)` with no producer content → `Failed` ("invalid deliberation state").
//! - Any role mismatch → `Failed` with a "protocol violation" reason.
//!
//! `Referee` is represented in the role enum but not yet part of the transition path.
//! Revision loops are not yet implemented. All provider calls are external and
//! represented as `DeliberationEffect::RunRole`.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;

pub use effect::DeliberationEffect;
pub use event::{DeliberationEvent, RoleResult};
pub use machine::DeliberationMachine;
pub use state::{DeliberationOutput, DeliberationRequest, DeliberationRole, DeliberationState};
