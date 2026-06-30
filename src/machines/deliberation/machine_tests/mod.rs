use super::super::effect::DeliberationEffect;
use super::super::event::{DeliberationEvent, ProducerValidationResult, RoleResult};
use super::super::state::DeliberationState;
use super::super::types::{
    CriticAdvisory, DeliberationRole, DeliberationTerminalOutput, RevisionFeedback,
};
use super::super::types::{DeliberationFailureReason, DeliberationRequest};
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
    let waiting_validator = step(
        state,
        DeliberationEvent::ProducerAccepted {
            content: content.to_string(),
            artifact_changed: false,
        },
    );
    assert!(matches!(
        waiting_validator.effects.as_slice(),
        [DeliberationEffect::ValidateProducer { .. }]
    ));
    assert!(matches!(
        &waiting_validator.state,
        DeliberationState::WaitingValidator { .. }
    ));
    step(
        waiting_validator.state,
        DeliberationEvent::ProducerValidationReturned {
            content: content.to_string(),
            result: ProducerValidationResult::Valid,
        },
    )
    .state
}
