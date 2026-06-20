use crate::engine::transition::Transition;

pub trait Machine {
    type State;
    type Event;
    type Effect;
    type Output;

    fn start_event(&self) -> Self::Event;

    fn transition(
        &self,
        state: Self::State,
        event: Self::Event,
    ) -> Transition<Self::State, Self::Effect>;

    fn handle_effect(&self, effect: Self::Effect) -> Self::Event;

    fn output(&self, state: &Self::State) -> Option<Self::Output>;
}

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
            None => machine.start_event(),
        };
    }
}
