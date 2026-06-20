//! Events for the demo machine.
//!
//! Events represent facts that have already happened — an agent responded, the
//! machine should start. The demo machine accepts exactly one response event
//! per stage.

use super::state::{CriticResponse, ProducerResponse, RefereeResponse};

/// Facts that drive the demo machine forward.
#[derive(Clone, Debug, PartialEq)]
pub enum DemoEvent {
    /// Synthetic tick used to start the first transition. The runner injects
    /// this when no prior effect has produced a real event.
    Start,

    /// The producer has finished and returned its text output.
    ProducerReturned { producer_response: ProducerResponse },

    /// The critic has finished evaluating the producer's output.
    CriticReturned { critic_response: CriticResponse },

    /// The referee has made its final decision.
    RefereeReturned { referee_response: RefereeResponse },
}
