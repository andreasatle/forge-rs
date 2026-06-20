//! Effects emitted by the scheduler machine.
//!
//! Effects are commands — things the scheduler *wants* to happen but cannot do
//! itself because they require I/O. The generic runner loop dispatches each
//! effect to the effect handler, which performs the work and converts the result
//! back into an event.
//!
//! This module does **not** own scheduler state or events; those live in
//! `state.rs` and `event.rs`.

use super::event::WorkOutput;
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
        /// The ID of the node to run, used to match the returned event.
        node_id: NodeId,
        /// Whether the node should plan or execute.
        kind: NodeKind,
        /// Natural-language description of what the node should accomplish.
        objective: String,
        /// The model capability level the runner should use.
        model_tier: ModelTier,
        /// Zero-based retry count; 0 on the first attempt.
        attempt: u32,
    },

    /// Dispatch the work produced by a node to the integration handler.
    ///
    /// The handler integrates the work and returns an `IntegrationReturned` event.
    /// The node remains `Integrating` until that event arrives.
    IntegrateWork {
        /// The ID of the node whose work is being integrated.
        node_id: NodeId,
        /// The work output to integrate.
        work: WorkOutput,
    },

    /// Signal that the entire run completed successfully.
    ///
    /// This effect is emitted alongside the transition to
    /// `SchedulerState::Complete`. The `RunMachine` (or `run_machine` during
    /// development) intercepts it to extract the final graph. It is never
    /// forwarded to `handle_effect`; reaching this effect in the handler is a
    /// bug.
    ReturnComplete {
        /// The final graph with every node in a terminal status.
        graph: RunGraph,
    },

    /// Signal that the run ended in an unrecoverable failure.
    ///
    /// Emitted alongside the transition to `SchedulerState::Failed`. Like
    /// `ReturnComplete`, it is a sentinel for the parent context and must not
    /// reach `handle_effect`.
    ReturnFailed {
        /// The graph at the point of failure, for post-mortem inspection.
        graph: RunGraph,
        /// A human-readable explanation of why the run was halted.
        reason: String,
    },
}
