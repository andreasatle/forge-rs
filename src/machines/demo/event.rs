use super::state::{CriticResponse, ProducerResponse, RefereeResponse};

#[derive(Clone, Debug, PartialEq)]
pub enum DemoEvent {
    Start,

    ProducerReturned {
        producer_response: ProducerResponse,
    },

    CriticReturned {
        critic_response: CriticResponse,
    },

    RefereeReturned {
        referee_response: RefereeResponse,
    },
}
