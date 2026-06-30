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
//! - `Ready + Start` → `Waiting(Producer)` + `RunRole(Producer)`.
//! - `Waiting(Producer) + RoleReturned(Producer, Accepted)` → `Waiting(Producer)` + `ValidateProducer`.
//! - `Waiting(Producer) + ProducerValidationReturned(Valid)` → `Waiting(Critic)` + `RunRole(Critic)`.
//! - `Waiting(Producer) + ProducerValidationReturned(Retry)` and validation retries remain
//!   → `Waiting(Producer)` with validation feedback + `RunRole(Producer, feedback)`.
//! - `Waiting(Producer) + ProducerValidationReturned(Retry)` and validation retries are exhausted
//!   → `Failed`.
//! - `Waiting(Producer) + RoleReturned(Producer, Rejected | Failed)` → `Failed`.
//! - `Waiting(Critic) + RoleReturned(Critic, Accepted)` → `Waiting(Referee)` + `RunRole(Referee)`.
//! - `Waiting(Critic) + RoleReturned(Critic, Rejected)` → `Waiting(Referee)` with advisory critic feedback.
//! - `Waiting(Critic) + RoleReturned(Critic, Failed)` → `Failed`.
//! - `Waiting(Critic)` with no producer content → `Failed` ("invalid deliberation state").
//! - `Waiting(Referee) + RoleReturned(Referee, Accepted)` → `Complete` with producer content.
//! - `Waiting(Referee) + RoleReturned(Referee, Rejected)` and revisions remain
//!   → `Waiting(Producer)` with updated `feedback`
//!   + `RunRole(Producer, feedback)`.
//! - `Waiting(Referee) + RoleReturned(Referee, Rejected)` and limit reached
//!   → `Failed` ("revision limit exhausted").
//! - `Waiting(Referee) + RoleReturned(Referee, Failed)` → `Failed` (no revision loop).
//! - `Waiting(Referee)` with missing producer or critic content → `Failed` ("invalid deliberation state").
//! - Any role mismatch → `Failed` with a "protocol violation" reason.

pub mod effect;
pub mod event;
pub mod handler;
pub mod machine;
mod planner_validation;
mod role_execution;
mod semantic_validation;
pub mod state;
mod telemetry;
mod validation;
mod work_validation;
mod workspace_context;

pub use effect::DeliberationEffect;
pub use event::{DeliberationEvent, ProducerValidationResult, RoleResult};
pub use handler::{DeliberationHandler, ProviderBackedDeliberationHandler};
pub use machine::DeliberationMachine;
pub use state::{
    ArtifactContext, DeliberationContext, DeliberationFailureReason, DeliberationOutput,
    DeliberationRequest, DeliberationRole, DeliberationState, DeliberationTerminalOutput,
    ProducerValidationState, RevisionFeedback, SelectedFileContent,
};
pub(crate) use validation::PlanValidationContext;
