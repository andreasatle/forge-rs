use super::super::effect::DeliberationEffect;
use super::super::event::{DeliberationEvent, RoleResult};
use super::super::state::{
    DeliberationRequest, DeliberationRole, DeliberationState, DeliberationTerminalOutput,
    RevisionFeedback,
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
            target_files: vec![],
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
