//! Scheduler run request type.

/// The external input to the scheduler.
///
/// Callers provide a `RunRequest` to start a new run instead of constructing a
/// `RunGraph` directly. `SchedulerMachine::initial_state` converts it into a
/// `SchedulerState::Active` containing a single root `Decomposition` node.
pub struct RunRequest {
    /// A natural-language description of what this run should accomplish.
    /// Becomes the objective of the root plan node.
    pub objective: String,
}
