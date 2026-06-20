#[derive(Debug)]
pub struct Transition<S, E> {
    pub state: S,
    pub effects: Vec<E>,
}
