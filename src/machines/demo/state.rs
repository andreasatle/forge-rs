//! Demo machine state types.
//!
//! The demo machine models a three-stage review pipeline: a producer generates
//! a response, a critic evaluates it, and a referee makes a final acceptance
//! decision. Each stage carries forward the outputs of all prior stages so that
//! later agents have full context.
//!
//! These types exist to demonstrate the machine pattern. They are not part of
//! the real Forge scheduler hierarchy.

/// The input to the demo pipeline. Everything the producer needs to know about
/// what it has been asked to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub name: String,
}

/// The raw output of the producer stage.
#[derive(Clone, Debug, PartialEq)]
pub struct ProducerResponse {
    pub text: String,
}

/// The evaluation produced by the critic after reviewing the producer's output.
#[derive(Clone, Debug, PartialEq)]
pub struct CriticResponse {
    pub text: String,
}

/// The referee's final verdict after seeing both the producer and critic outputs.
#[derive(Clone, Debug, PartialEq)]
pub struct RefereeResponse {
    pub text: String,
}

/// All three stage outputs collected into the final result.
///
/// `TaskResult` is what the caller receives when `run_machine` returns. It
/// holds a complete record of the pipeline execution so that callers can
/// inspect every stage's contribution.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskResult {
    pub task: Task,
    pub producer_response: ProducerResponse,
    pub critic_response: CriticResponse,
    pub referee_response: RefereeResponse,
}

/// The lifecycle of the demo machine.
///
/// Each variant carries exactly what was accumulated up to that point. State
/// only moves forward; earlier stage outputs are always present in later states.
#[derive(Clone, Debug, PartialEq)]
pub enum DemoState {
    /// The pipeline has not started yet; only the original task is available.
    NotStarted { task: Task },

    /// The producer has responded. The critic has not yet been called.
    PostProducer {
        task: Task,
        producer_response: ProducerResponse,
    },

    /// The critic has responded. The referee has not yet been called.
    PostCritic {
        task: Task,
        producer_response: ProducerResponse,
        critic_response: CriticResponse,
    },

    /// All three stages have completed. This is the terminal state; `output`
    /// will extract the `TaskResult` from here.
    PostReferee { result: TaskResult },
}
