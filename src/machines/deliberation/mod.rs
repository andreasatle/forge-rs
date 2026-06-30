//! DeliberationMachine — Producer → Critic → Referee deliberation pipeline
//! with bounded revision loops.
//!
//! A single `DeliberationRequest` enters; a `DeliberationOutput` (or failure) exits.
//! Final output is always the producer content; critic and referee do not replace it.
//!
//! The transition algebra is:
//!
//! ```text
//! (DeliberationState, DeliberationEvent) -> (DeliberationState, DeliberationEffect)
//! ```
//!
//! ## Role result semantics
//!
//! Role results distinguish semantic outcomes from infrastructure failures:
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
//! - `WaitingValidator + ProducerValidationAccepted` → `WaitingCritic` + `RunRole(Critic)`.
//! - `WaitingValidator + ProducerValidationRejected` and validation retries remain
//!   → `WaitingProducer` with validation feedback + `RunRole(Producer, feedback)`.
//! - `WaitingValidator + ProducerValidationRejected` and validation retries are exhausted
//!   → `Failed`.
//! - `WaitingProducer + ProducerRejected | ProducerFailed` → `Failed`.
//! - `WaitingCritic + CriticAccepted` → `WaitingReferee` + `RunRole(Referee)`.
//! - `WaitingCritic + CriticRejected` → `WaitingReferee` with advisory critic feedback.
//! - `WaitingCritic + CriticFailed` → `Failed`.
//! - `WaitingReferee + RefereeAccepted` → `Complete` with producer content.
//! - `WaitingReferee + RefereeRejected` and revisions remain
//!   → `WaitingProducer` with updated `feedback`
//!   + `RunRole(Producer, feedback)`.
//! - `WaitingReferee + RefereeRejected` and limit reached
//!   → `Failed` ("revision limit exhausted").
//! - `WaitingReferee + RefereeFailed` → `Failed` (no revision loop).
//! - Any role mismatch → `Failed` with a "protocol violation" reason.

pub mod effect;
pub mod event;
pub mod handler;
pub mod machine;
pub mod state;
pub mod types;

pub use effect::DeliberationEffect;
pub use event::{DeliberationEvent, ProducerValidationRetry};
pub(crate) use handler::PlanValidationContext;
pub use handler::{DeliberationHandler, ProviderBackedDeliberationHandler};
pub use machine::DeliberationMachine;
pub use state::DeliberationState;
pub use types::{
    ArtifactContext, CriticAdvisory, DeliberationContext, DeliberationFailureReason,
    DeliberationOutput, DeliberationRequest, DeliberationRole, DeliberationTerminalOutput,
    RevisionFeedback, SelectedFileContent,
};
