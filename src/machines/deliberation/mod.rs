//! DeliberationMachine — Producer → Critic → Referee deliberation pipeline
//! with bounded revision loops.
//!
//! A single `DeliberationRequest` enters; a `DeliberationOutput` (or failure) exits.
//! Final output is always the producer content; critic and referee do not replace it.
//!
//! - `Ready + Start` → `Waiting(Producer)` + `RunRole(Producer)`.
//! - `Waiting(Producer) + RoleReturned(Producer, Accepted)` → `Waiting(Critic)` + `RunRole(Critic)`.
//! - `Waiting(Producer) + RoleReturned(Producer, Rejected)` → `Failed`.
//! - `Waiting(Critic) + RoleReturned(Critic, Accepted)` → `Waiting(Referee)` + `RunRole(Referee)`.
//! - `Waiting(Critic) + RoleReturned(Critic, Rejected)` → `Failed`.
//! - `Waiting(Critic)` with no producer content → `Failed` ("invalid deliberation state").
//! - `Waiting(Referee) + RoleReturned(Referee, Accepted)` → `Complete` with producer content.
//! - `Waiting(Referee) + RoleReturned(Referee, Rejected)` and revisions remain
//!   → `Waiting(Producer)` with incremented `revision_count` and updated `feedback`
//!   + `RunRole(Producer, feedback)`.
//! - `Waiting(Referee) + RoleReturned(Referee, Rejected)` and limit reached
//!   → `Failed` ("revision limit exhausted").
//! - `Waiting(Referee)` with missing producer or critic content → `Failed` ("invalid deliberation state").
//! - Any role mismatch → `Failed` with a "protocol violation" reason.

pub mod effect;
pub mod event;
pub mod machine;
pub mod state;

pub use effect::DeliberationEffect;
pub use event::{DeliberationEvent, RoleResult};
pub use machine::DeliberationMachine;
pub use state::{
    DeliberationOutput, DeliberationRequest, DeliberationRole, DeliberationState,
    DeliberationTerminalOutput, RevisionFeedback,
};
