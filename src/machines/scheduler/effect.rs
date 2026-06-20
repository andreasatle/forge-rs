//! Effects emitted by the scheduler machine.
//!
//! Effects are commands — things the scheduler *wants* to happen but cannot do
//! itself because they require I/O. The generic runner loop dispatches each
//! effect to the effect handler, which performs the work and converts the result
//! back into an event.
//!
//! This module does **not** own scheduler state or events; those live in
//! `state.rs` and `event.rs`.

use super::state::{ModelTier, NodeId, NodeKind, RunGraph};

/// Commands that the scheduler emits to the outside world.
///
/// Transition functions are pure, so they cannot run nodes or signal
/// completion directly. Instead they emit `SchedulerEffect` values that the
/// handler layer executes on their behalf.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEffect {
    /// Dispatch a single node to a runner for execution.
    ///
    /// The handler is responsible for running the node with the given
    /// parameters and returning a `NodeReturned` event when it finishes.
    /// Carrying `kind`, `objective`, `model_tier`, and `attempt` here means
    /// the handler does not need to re-read the graph.
    RunNode {
        node_id: NodeId,
        kind: NodeKind,
        objective: String,
        model_tier: ModelTier,
        attempt: u32,
    },

    /// Signal that the entire run completed successfully.
    ///
    /// This effect is emitted alongside the transition to
    /// `SchedulerState::Complete`. The `RunMachine` (or `run_machine` during
    /// development) intercepts it to extract the final graph. It is never
    /// forwarded to `handle_effect`; reaching this effect in the handler is a
    /// bug.
    ReturnComplete { graph: RunGraph },

    /// Signal that the run ended in an unrecoverable failure.
    ///
    /// Emitted alongside the transition to `SchedulerState::Failed`. Like
    /// `ReturnComplete`, it is a sentinel for the parent context and must not
    /// reach `handle_effect`.
    ReturnFailed { graph: RunGraph, reason: String },
}
