//! DeliberationMachine — Producer → Critic → Referee deliberation pipeline
//! with bounded revision loops.
//!
//! A single `DeliberationRequest` enters; a `DeliberationOutput` (or failure) exits.
//! Final output is always the producer content; critic and referee do not replace it.
//!
//! ## Role result semantics
//!
//! `RoleResult` distinguishes semantic outcomes from infrastructure failures:
//!
//! - `Accepted` — role completed successfully; content is acceptable.
//! - `Rejected` — role completed successfully but rejected the content.
//!   Producer rejection is terminal. Critic rejection is advisory and proceeds
//!   to the Referee. Referee rejection triggers a revision loop while budget
//!   remains, otherwise it terminates the pipeline.
//! - `Failed` — role could not execute (timeout, provider unavailable, auth error,
//!   malformed response, etc.). Always a terminal failure for every role.
//!   A `Failed` Referee result must never enter the revision loop.
//!
//! ## Transitions
//!
//! - `Ready + Start` → `WaitingProducer` + `RunRole(Producer)`.
//! - `WaitingProducer + ProducerAccepted` → `WaitingValidator` + `ValidateProducer`.
//! - `WaitingValidator + ProducerValidationReturned(Valid)` → `WaitingCritic` + `RunRole(Critic)`.
//! - `WaitingValidator + ProducerValidationReturned(Retry)` and validation retries remain
//!   → `WaitingProducer` with validation feedback + `RunRole(Producer, feedback)`.
//! - `WaitingValidator + ProducerValidationReturned(Retry)` and validation retries are exhausted
//!   → `Failed`.
//! - `WaitingProducer + RoleReturned(Producer, Rejected | Failed)` → `Failed`.
//! - `WaitingCritic + RoleReturned(Critic, Accepted)` → `WaitingReferee` + `RunRole(Referee)`.
//! - `WaitingCritic + RoleReturned(Critic, Rejected)` → `WaitingReferee` with advisory critic feedback.
//! - `WaitingCritic + RoleReturned(Critic, Failed)` → `Failed`.
//! - `WaitingReferee + RoleReturned(Referee, Accepted)` → `Complete` with producer content.
//! - `WaitingReferee + RoleReturned(Referee, Rejected)` and revisions remain
//!   → `WaitingProducer` with updated `feedback`
//!   + `RunRole(Producer, feedback)`.
//! - `WaitingReferee + RoleReturned(Referee, Rejected)` and limit reached
//!   → `Failed` ("revision limit exhausted").
//! - `WaitingReferee + RoleReturned(Referee, Failed)` → `Failed` (no revision loop).
//! - Any role mismatch → `Failed` with a "protocol violation" reason.

pub mod effect;
pub mod event;
pub mod failure;
pub mod handler;
pub mod machine;
mod planner_validation;
pub mod request;
mod role_execution;
mod semantic_validation;
pub mod state;
mod telemetry;
pub mod types;
mod validation;
mod work_validation;
mod workspace_context;

pub use effect::DeliberationEffect;
pub use event::{DeliberationEvent, ProducerValidationResult, RoleResult};
pub use failure::DeliberationFailureReason;
pub use handler::{DeliberationHandler, ProviderBackedDeliberationHandler};
pub use machine::DeliberationMachine;
pub use request::{ArtifactContext, DeliberationContext, DeliberationRequest, SelectedFileContent};
pub use state::DeliberationState;
pub use types::{
    CriticAdvisory, DeliberationOutput, DeliberationRole, DeliberationTerminalOutput,
    RevisionFeedback,
};
pub(crate) use validation::PlanValidationContext;
