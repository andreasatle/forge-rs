use crate::engine::transition::Transition;

/// A state machine that can be driven by the generic [`run_machine`] loop.
///
/// Implementors define four associated types that describe their domain, then
/// fill in four methods that together form a complete reactive component:
///
/// ```text
/// start_event  — bootstraps the first tick
/// transition   — pure: state + event  →  next_state + effects
/// handle_effect — impure: executes one effect and returns the next event
/// output        — recognises terminal states and extracts the final value
/// ```
///
/// # Invariants
///
/// - `transition` must be pure: no I/O, no mutation of shared state.
/// - `handle_effect` is the only site where I/O is allowed.
/// - `output` returning `Some` halts the loop immediately.
pub trait Machine {
    /// The machine's durable state, updated on every transition.
    type State;
    /// Inputs that drive the machine forward (facts, responses, ticks).
    type Event;
    /// Commands emitted by transitions; executed by [`handle_effect`](Machine::handle_effect).
    type Effect;
    /// The value extracted when the machine reaches a terminal state.
    type Output;

    /// Returns the event used to bootstrap the machine on the first tick, and
    /// again whenever a transition produces no effects (see [`run_machine`]).
    fn start_event(&self) -> Self::Event;

    /// Pure transition function. Given the current state and an event, returns
    /// the next state and any effects to dispatch. Must not perform I/O.
    fn transition(
        &self,
        state: Self::State,
        event: Self::Event,
    ) -> Transition<Self::State, Self::Effect>;

    /// Executes one effect and converts the external result into the next event.
    /// This is the only method that may perform I/O.
    fn handle_effect(&self, effect: Self::Effect) -> Self::Event;

    /// Inspects the current state and returns `Some(output)` if the machine has
    /// reached a terminal state, `None` to continue the loop.
    fn output(&self, state: &Self::State) -> Option<Self::Output>;
}

/// Drive a machine to completion and return its output.
///
/// The loop follows a simple protocol:
///
/// ```text
/// 1. Send start_event to kick off the first transition.
/// 2. transition(state, event)  →  next_state + effects
/// 3. If output(next_state) is Some, return it — the machine is done.
/// 4. If effects is non-empty, dispatch the first effect through handle_effect
///    to get the next event; otherwise re-send start_event as a free tick.
/// 5. Repeat from step 2.
/// ```
///
/// Re-sending `start_event` when there are no effects lets machines advance
/// through pure bookkeeping steps — states that need a nudge but not a real
/// external result — without blocking.
pub fn run_machine<M>(machine: M, mut state: M::State) -> M::Output
where
    M: Machine,
{
    let mut event = machine.start_event();

    loop {
        let transition = machine.transition(state, event);
        state = transition.state;

        if let Some(output) = machine.output(&state) {
            return output;
        }

        event = match transition.effects.into_iter().next() {
            Some(effect) => machine.handle_effect(effect),
            // No effects produced: nudge the machine forward with another tick.
            None => machine.start_event(),
        };
    }
}
