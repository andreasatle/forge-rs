use super::state::{CriticResponse, ProducerResponse, Task};

#[derive(Clone, Debug, PartialEq)]
pub enum DemoEffect {
    CallProducer {
        task: Task,
    },

    CallCritic {
        task: Task,
        producer_response: ProducerResponse,
    },

    CallReferee {
        task: Task,
        producer_response: ProducerResponse,
        critic_response: CriticResponse,
    },
}
