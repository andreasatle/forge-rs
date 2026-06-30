//! Demo machine state types.
//!
//! The demo machine models a three-stage review pipeline: a producer generates
//! a response, a critic evaluates it, and a referee makes a final acceptance
//! decision. Each stage carries forward the outputs of all prior stages so that
//! later agents have full context.
//!
//! These types exist to demonstrate the machine pattern. They are not part of
//! the real Forge scheduler hierarchy.

pub use super::types::{CriticResponse, ProducerResponse, RefereeResponse, Task, TaskResult};

/// The lifecycle of the demo machine.
///
/// Each variant carries exactly what was accumulated up to that point. State
/// only moves forward; earlier stage outputs are always present in later states.
#[derive(Clone, Debug, PartialEq)]
pub enum DemoState {
    /// The pipeline has not started yet; only the original task is available.
    NotStarted {
        /// The task waiting to enter the pipeline.
        task: Task,
    },

    /// The producer has responded. The critic has not yet been called.
    PostProducer {
        /// The original task, carried forward for later stages.
        task: Task,
        /// The producer's output, ready to be sent to the critic.
        producer_response: ProducerResponse,
    },

    /// The critic has responded. The referee has not yet been called.
    PostCritic {
        /// The original task, carried forward for the referee.
        task: Task,
        /// The producer's output, carried forward for the referee.
        producer_response: ProducerResponse,
        /// The critic's evaluation, ready to be sent to the referee.
        critic_response: CriticResponse,
    },

    /// All three stages have completed. This is the terminal state; `output`
    /// will extract the `TaskResult` from here.
    PostReferee {
        /// The complete pipeline result, including all stage outputs.
        result: TaskResult,
    },
}
