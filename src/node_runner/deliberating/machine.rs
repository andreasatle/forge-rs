//! Machine adapter for provider-backed deliberation.

use crate::engine::{Machine, Transition};
use crate::machines::deliberation::{
    DeliberationEffect, DeliberationEvent, DeliberationMachine, DeliberationState,
    DeliberationTerminalOutput, ProviderBackedDeliberationHandler,
};
use crate::providers::ProviderClient;
use crate::telemetry::TelemetrySink;

pub(crate) struct DeliberatingMachine<'a, P: ProviderClient> {
    pub(crate) handler: ProviderBackedDeliberationHandler<&'a P>,
    pub(crate) telemetry: &'a dyn TelemetrySink,
}

impl<'a, P: ProviderClient> Machine for DeliberatingMachine<'a, P> {
    type State = DeliberationState;
    type Event = DeliberationEvent;
    type Effect = DeliberationEffect;
    type Output = DeliberationTerminalOutput;

    fn start_event(&self) -> DeliberationEvent {
        DeliberationMachine.start_event()
    }

    fn transition(
        &self,
        state: DeliberationState,
        event: DeliberationEvent,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        DeliberationMachine.transition(state, event)
    }

    fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        self.handler
            .handle_effect_with_telemetry(effect, self.telemetry)
    }

    fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
        DeliberationMachine.output(state)
    }

    fn name(&self) -> String {
        "DeliberationMachine".to_string()
    }
}
