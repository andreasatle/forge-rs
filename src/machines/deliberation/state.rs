//! DeliberationMachine state enum.
//!
//! This module owns only `DeliberationState`, the phase enum for the
//! deliberation state machine. Supporting payload types (`RevisionFeedback`,
//! `CriticAdvisory`) live in `types.rs`.

use crate::machines::scheduler::FailureKind;

use super::request::DeliberationRequest;

use super::failure::DeliberationFailureReason;
use super::types::{CriticAdvisory, DeliberationOutput, RevisionFeedback};

/// The lifecycle of the deliberation machine.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliberationState {
    /// The machine has a request and is waiting for `Start`.
    Ready {
        /// The request that will be processed once the machine starts.
        request: DeliberationRequest,
    },

    /// The Producer has been dispatched; the machine is waiting for its result.
    WaitingProducer {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
        /// Number of producer semantic validation rejections already retried.
        /// Carried forward so the total retry budget is shared across the revision loop.
        validation_attempt: usize,
    },

    /// The Validator is running against accepted Producer content.
    WaitingValidator {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// The content accepted by the Producer, under validation.
        producer_content: String,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
        /// Number of producer semantic validation rejections already retried.
        validation_attempt: usize,
    },

    /// The Critic has been dispatched; the machine is waiting for its result.
    WaitingCritic {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// Content accepted by the Producer (always present at this stage).
        producer_content: String,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
    },

    /// The Referee has been dispatched; the machine is waiting for its result.
    WaitingReferee {
        /// The original request, carried forward for later stages.
        request: DeliberationRequest,
        /// Content accepted by the Producer (always present at this stage).
        producer_content: String,
        /// Advisory result from the Critic stage (always present at this stage).
        critic_advisory: CriticAdvisory,
        /// Revision feedback accumulated from prior Referee rejections.
        feedback: Vec<RevisionFeedback>,
    },

    /// The pipeline finished successfully. Terminal state.
    Complete {
        /// The accepted output from the pipeline.
        output: DeliberationOutput,
    },

    /// The pipeline failed. Terminal state.
    Failed {
        /// Machine-readable failure cause.
        kind: FailureKind,
        /// Machine-readable terminal failure reason.
        reason: DeliberationFailureReason,
        /// Human-readable diagnostic text.
        message: String,
    },
}
