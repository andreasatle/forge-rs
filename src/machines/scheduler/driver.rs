//! Bridges the pure [`SchedulerMachine`] and the impure [`SchedulerHandler`]
//! into something [`crate::engine::run_machine`] can drive.
//!
//! Mirrors the role `DeliberatingMachine` plays for the deliberation machine
//! (in `node_runner::deliberating`): neither `SchedulerMachine` nor
//! `SchedulerHandler` implements the generic [`Machine`](crate::engine::Machine)
//! trait itself, so a small owning wrapper composes them for the engine's
//! runner loop.

use crate::engine::{Machine, Transition, run_machine, run_machine_with_telemetry};
use crate::telemetry::TelemetrySink;

use super::effect::SchedulerEffect;
use super::event::SchedulerEvent;
use super::handler::SchedulerHandler;
use super::machine::{SchedulerMachine, SchedulerTerminalOutput};
use super::state::SchedulerState;
use crate::node_runner::NodeRunner;

struct SchedulerDriver<R> {
    handler: SchedulerHandler<R>,
}

impl<R: NodeRunner> Machine for SchedulerDriver<R> {
    type State = SchedulerState;
    type Event = SchedulerEvent;
    type Effect = SchedulerEffect;
    type Output = SchedulerTerminalOutput;

    fn name(&self) -> String {
        "SchedulerMachine".to_string()
    }

    fn start_event(&self) -> SchedulerEvent {
        SchedulerMachine.start_event()
    }

    fn transition(
        &self,
        state: SchedulerState,
        event: SchedulerEvent,
    ) -> Transition<SchedulerState, SchedulerEffect> {
        self.handler.transition(state, event)
    }

    fn handle_effect(&self, effect: SchedulerEffect) -> SchedulerEvent {
        self.handler.handle_effect(effect)
    }

    fn output(&self, state: &SchedulerState) -> Option<SchedulerTerminalOutput> {
        SchedulerMachine.output(state)
    }
}

/// Drive a scheduler run to completion, discarding telemetry.
pub fn run_scheduler<R: NodeRunner>(
    handler: SchedulerHandler<R>,
    state: SchedulerState,
) -> SchedulerTerminalOutput {
    run_machine(SchedulerDriver { handler }, state)
}

/// Drive a scheduler run to completion, recording telemetry at each step.
pub fn run_scheduler_with_telemetry<R: NodeRunner>(
    handler: SchedulerHandler<R>,
    state: SchedulerState,
    telemetry: &dyn TelemetrySink,
) -> (SchedulerTerminalOutput, SchedulerHandler<R>) {
    let (output, driver) =
        run_machine_with_telemetry(SchedulerDriver { handler }, state, telemetry);
    (output, driver.handler)
}
