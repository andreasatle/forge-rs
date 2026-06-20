/// The return value of every machine transition.
///
/// A `Transition` pairs the **next state** with a list of **effects** — commands
/// that the runner will hand off to the effect handler before the next event
/// arrives. Separating state from effects keeps transition functions pure: they
/// compute what should happen without performing any I/O themselves.
///
/// An empty `effects` vec is valid and signals that no external action is needed
/// before the next event (the runner will re-send `start_event` as a free tick).
#[derive(Debug)]
pub struct Transition<S, E> {
    /// The machine's new durable state after the transition.
    pub state: S,
    /// Side-effect commands produced by this transition, in dispatch order.
    /// The runner processes at most one effect per tick; if you emit multiple,
    /// only the first is dispatched and the rest are currently dropped.
    pub effects: Vec<E>,
}
