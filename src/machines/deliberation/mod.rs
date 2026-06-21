//! DeliberationMachine — Producer / Critic / Referee revision loop.
//!
//! This machine owns the multi-role deliberation pipeline. A single
//! `DeliberationRequest` enters; a `DeliberationOutput` (or failure) exits.
//!
//! **Phase 1** wires only the Producer role:
//! - `Ready + Start` dispatches `RunRole(Producer)` and enters `Waiting`.
//! - `Waiting(Producer) + RoleReturned(Accepted)` → `Complete`.
//! - `Waiting(Producer) + RoleReturned(Rejected)` → `Failed`.
//! - Any role mismatch → `Failed` with a "protocol violation" reason.
//!
//! Critic and Referee transitions come in later phases. All provider calls are
//! external to this machine and represented as `DeliberationEffect::RunRole`.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;

pub use effect::DeliberationEffect;
pub use event::{DeliberationEvent, RoleResult};
pub use machine::DeliberationMachine;
pub use state::{DeliberationOutput, DeliberationRequest, DeliberationRole, DeliberationState};
