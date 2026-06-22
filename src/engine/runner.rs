use crate::engine::transition::Transition;
use crate::telemetry::{NoopTelemetry, TelemetryEvent, TelemetrySink};

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

/// Extracts the short type name (last `::` segment) for use in telemetry.
fn short_type_name<T>() -> String {
    let full = std::any::type_name::<T>();
    full.rsplit("::").next().unwrap_or(full).to_string()
}

/// Drive a machine to completion, recording telemetry at each step.
///
/// The loop follows a simple protocol:
///
/// ```text
/// 1. Record MachineStarted.
/// 2. Send start_event to kick off the first transition.
/// 3. Record StateEntered and EventReceived.
/// 4. transition(state, event)  →  next_state + effects
/// 5. If output(next_state) is Some, return it — the machine is done.
/// 6. If effects is non-empty, record EffectEmitted, dispatch through
///    handle_effect to get the next event; otherwise re-send start_event.
/// 7. Repeat from step 3.
/// ```
///
/// # Engine invariant
///
/// A transition may emit **zero or one** effect per tick. Emitting two or more
/// effects is treated as a bug and causes an immediate panic.
pub fn run_machine_with_telemetry<M, T>(machine: M, mut state: M::State, telemetry: &T) -> M::Output
where
    M: Machine,
    M::State: std::fmt::Debug,
    M::Event: std::fmt::Debug,
    M::Effect: std::fmt::Debug,
    T: TelemetrySink,
{
    let machine_name = short_type_name::<M>();
    telemetry.record(TelemetryEvent::MachineStarted {
        machine: machine_name.clone(),
    });

    let mut event = machine.start_event();

    loop {
        telemetry.record(TelemetryEvent::StateEntered {
            machine: machine_name.clone(),
            state: format!("{state:#?}"),
        });
        telemetry.record(TelemetryEvent::EventReceived {
            machine: machine_name.clone(),
            event: format!("{event:#?}"),
        });

        let transition = machine.transition(state, event);
        state = transition.state;

        if let Some(output) = machine.output(&state) {
            return output;
        }

        let mut effects = transition.effects.into_iter();
        event = match effects.next() {
            Some(effect) => {
                assert!(
                    effects.next().is_none(),
                    "Machine emitted multiple effects but the engine currently supports exactly one effect per transition."
                );
                telemetry.record(TelemetryEvent::EffectEmitted {
                    machine: machine_name.clone(),
                    effect: format!("{effect:#?}"),
                });
                machine.handle_effect(effect)
            }
            None => machine.start_event(),
        };
    }
}

/// Drive a machine to completion and return its output.
///
/// Equivalent to calling [`run_machine_with_telemetry`] with [`NoopTelemetry`].
/// All telemetry events are silently discarded.
///
/// See [`run_machine_with_telemetry`] for the full execution protocol and
/// the single-effect invariant.
pub fn run_machine<M>(machine: M, initial_state: M::State) -> M::Output
where
    M: Machine,
    M::State: std::fmt::Debug,
    M::Event: std::fmt::Debug,
    M::Effect: std::fmt::Debug,
{
    run_machine_with_telemetry(machine, initial_state, &NoopTelemetry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::transition::Transition;
    use crate::telemetry::{FileTelemetry, VecTelemetry};
    use std::path::PathBuf;

    // Minimal machine that emits two effects on the first tick, then halts.
    struct MultiEffectMachine;

    #[derive(Clone, Copy, Debug)]
    enum MeState {
        Start,
        Done,
    }

    impl Machine for MultiEffectMachine {
        type State = MeState;
        type Event = ();
        type Effect = &'static str;
        type Output = ();

        fn start_event(&self) -> () {
            ()
        }

        fn transition(&self, state: MeState, _event: ()) -> Transition<MeState, &'static str> {
            match state {
                MeState::Start => Transition {
                    state: MeState::Done,
                    effects: vec!["effect-one", "effect-two"],
                },
                MeState::Done => Transition {
                    state: MeState::Done,
                    effects: vec![],
                },
            }
        }

        fn handle_effect(&self, _effect: &'static str) -> () {
            ()
        }

        fn output(&self, state: &MeState) -> Option<()> {
            match state {
                MeState::Done => None, // never reached after panic
                MeState::Start => None,
            }
        }
    }

    #[test]
    #[should_panic(expected = "multiple effects")]
    fn run_machine_panics_on_multiple_effects() {
        run_machine(MultiEffectMachine, MeState::Start);
    }

    // Minimal two-step machine: Start emits one effect, Done is terminal.
    struct SimpleCountMachine;

    #[derive(Debug)]
    enum ScState {
        Start,
        Done,
    }

    #[derive(Debug)]
    enum ScEvent {
        Kick,
        Done,
    }

    #[derive(Debug)]
    struct ScEffect;

    impl Machine for SimpleCountMachine {
        type State = ScState;
        type Event = ScEvent;
        type Effect = ScEffect;
        type Output = &'static str;

        fn start_event(&self) -> ScEvent {
            ScEvent::Kick
        }

        fn transition(&self, state: ScState, event: ScEvent) -> Transition<ScState, ScEffect> {
            match (state, event) {
                (ScState::Start, ScEvent::Kick) => Transition {
                    state: ScState::Start,
                    effects: vec![ScEffect],
                },
                (ScState::Start, ScEvent::Done) => Transition {
                    state: ScState::Done,
                    effects: vec![],
                },
                (ScState::Done, _) => Transition {
                    state: ScState::Done,
                    effects: vec![],
                },
            }
        }

        fn handle_effect(&self, _effect: ScEffect) -> ScEvent {
            ScEvent::Done
        }

        fn output(&self, state: &ScState) -> Option<&'static str> {
            match state {
                ScState::Done => Some("finished"),
                ScState::Start => None,
            }
        }
    }

    #[test]
    fn run_machine_wrapper_still_works() {
        let result = run_machine(SimpleCountMachine, ScState::Start);
        assert_eq!(result, "finished");
    }

    #[test]
    fn vec_telemetry_records_machine_events() {
        let sink = VecTelemetry::new();
        run_machine_with_telemetry(SimpleCountMachine, ScState::Start, &sink);
        let events = sink.into_events();
        assert!(matches!(events[0], TelemetryEvent::MachineStarted { .. }));
        assert!(matches!(events[1], TelemetryEvent::StateEntered { .. }));
        assert!(matches!(events[2], TelemetryEvent::EventReceived { .. }));
        assert!(matches!(events[3], TelemetryEvent::EffectEmitted { .. }));
    }

    #[test]
    fn run_machine_with_file_telemetry_writes_trace_files() {
        let dir: PathBuf = std::env::temp_dir().join("forge-runner-trace-test");
        let _ = std::fs::remove_dir_all(&dir);
        let sink = FileTelemetry::new(dir.clone()).unwrap();
        run_machine_with_telemetry(SimpleCountMachine, ScState::Start, &sink);
        assert!(dir.join("000001-machine-started.txt").exists());
        assert!(dir.join("000002-state-entered.txt").exists());
        assert!(dir.join("000003-event-received.txt").exists());
        assert!(dir.join("000004-effect-emitted.txt").exists());
    }
}
