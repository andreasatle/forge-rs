//! Effects emitted by the demo machine.
//!
//! Each effect is a command to call one of the three agents. The machine
//! carries all prior outputs in the effect payload so the handler does not
//! need to re-read state — it has everything needed to construct the prompt.

use super::state::{CriticResponse, ProducerResponse, Task};

/// Commands that the demo machine emits to the handler layer.
#[derive(Clone, Debug, PartialEq)]
pub enum DemoEffect {
    /// Ask the producer to generate a response for `task`.
    CallProducer { task: Task },

    /// Ask the critic to evaluate `producer_response` in the context of `task`.
    CallCritic {
        task: Task,
        producer_response: ProducerResponse,
    },

    /// Ask the referee to make a final decision given the full pipeline context.
    CallReferee {
        task: Task,
        producer_response: ProducerResponse,
        critic_response: CriticResponse,
    },
}
