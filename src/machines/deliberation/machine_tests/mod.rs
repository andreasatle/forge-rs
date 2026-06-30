use super::super::effect::DeliberationEffect;
use super::super::event::{DeliberationEvent, ProducerValidationResult, RoleResult};
use super::super::request::DeliberationRequest;
use super::super::state::{
    CriticAdvisory, DeliberationFailureReason, DeliberationRole, DeliberationState,
    DeliberationTerminalOutput, ProducerValidationState, RevisionFeedback,
};
use super::DeliberationMachine;
use crate::engine::{Machine, Transition, run_machine};
use crate::machines::scheduler::FailureKind;

mod critic;
mod failure;
mod producer;
mod referee;
mod revision;
mod run_machine;

fn ready(objective: &str) -> DeliberationState {
    DeliberationState::Ready {
        request: DeliberationRequest {
            objective: objective.to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 0,
        },
    }
}

fn machine() -> DeliberationMachine {
    DeliberationMachine
}

fn step(
    state: DeliberationState,
    event: DeliberationEvent,
) -> Transition<DeliberationState, DeliberationEffect> {
    machine().transition(state, event)
}

fn producer_accepts(state: DeliberationState, content: &str) -> DeliberationState {
    let validating = step(
        state,
        DeliberationEvent::ProducerAccepted {
            content: content.to_string(),
            artifact_changed: false,
        },
    );
    assert!(matches!(
        validating.effects.as_slice(),
        [DeliberationEffect::ValidateProducer { .. }]
    ));
    step(
        validating.state,
        DeliberationEvent::ProducerValidationReturned {
            content: content.to_string(),
            result: ProducerValidationResult::Valid,
        },
    )
    .state
}
