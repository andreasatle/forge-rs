//! Demo machine — transition logic and stub handlers.
//!
//! This machine implements the three-stage producer/critic/referee pipeline
//! defined in the sibling state, event, and effect modules. It is used to
//! verify that the generic `Machine` trait and `run_machine` runner work
//! correctly before the scheduler is exercised.
//!
//! The `handle_effect` implementation sleeps to simulate latency. In a real
//! machine the sleep would be replaced by actual provider calls.

use std::thread::sleep;
use std::time::Duration;

use crate::engine::{Machine, Transition};

use super::effect::DemoEffect;
use super::event::DemoEvent;
use super::state::{CriticResponse, DemoState, ProducerResponse, RefereeResponse, TaskResult};

/// The demo machine. Carries no state of its own; all data travels in
/// `DemoState` variants.
pub struct DemoMachine;

impl Machine for DemoMachine {
    type State = DemoState;
    type Event = DemoEvent;
    type Effect = DemoEffect;
    type Output = TaskResult;

    fn start_event(&self) -> Self::Event {
        DemoEvent::Start
    }

    fn transition(
        &self,
        state: Self::State,
        event: Self::Event,
    ) -> Transition<Self::State, Self::Effect> {
        println!("STATE: {state:#?}");
        println!("EVENT: {event:#?}");

        match (state, event) {
            // Bootstrap: Start kicks off the pipeline by emitting CallProducer.
            // The state stays NotStarted; the transition to PostProducer happens
            // when ProducerReturned arrives in the next arm.
            (DemoState::NotStarted { task }, DemoEvent::Start) => Transition {
                state: DemoState::NotStarted { task: task.clone() },
                effects: vec![DemoEffect::CallProducer { task }],
            },

            // Producer returned: store its response and immediately call the critic.
            (DemoState::NotStarted { task }, DemoEvent::ProducerReturned { producer_response }) => {
                Transition {
                    state: DemoState::PostProducer {
                        task: task.clone(),
                        producer_response: producer_response.clone(),
                    },
                    effects: vec![DemoEffect::CallCritic {
                        task,
                        producer_response,
                    }],
                }
            }

            // Critic returned: store its response and immediately call the referee.
            // Both prior outputs are forwarded so the referee has full context.
            (
                DemoState::PostProducer {
                    task,
                    producer_response,
                },
                DemoEvent::CriticReturned { critic_response },
            ) => Transition {
                state: DemoState::PostCritic {
                    task: task.clone(),
                    producer_response: producer_response.clone(),
                    critic_response: critic_response.clone(),
                },
                effects: vec![DemoEffect::CallReferee {
                    task,
                    producer_response,
                    critic_response,
                }],
            },

            // Referee returned: assemble the final result and move to the terminal state.
            // No effects — the runner will call `output` next and halt the loop.
            (
                DemoState::PostCritic {
                    task,
                    producer_response,
                    critic_response,
                },
                DemoEvent::RefereeReturned { referee_response },
            ) => {
                let result = TaskResult {
                    task,
                    producer_response,
                    critic_response,
                    referee_response,
                };

                Transition {
                    state: DemoState::PostReferee { result },
                    effects: vec![],
                }
            }

            (state, event) => {
                panic!("invalid transition: state={state:#?}, event={event:#?}");
            }
        }
    }

    fn handle_effect(&self, effect: Self::Effect) -> Self::Event {
        println!("EFFECT: {effect:#?}");

        match effect {
            DemoEffect::CallProducer { task } => {
                sleep(Duration::from_secs(1));

                DemoEvent::ProducerReturned {
                    producer_response: ProducerResponse {
                        text: format!("producer handled '{}'", task.name),
                    },
                }
            }

            DemoEffect::CallCritic {
                task,
                producer_response,
            } => {
                sleep(Duration::from_secs(1));

                DemoEvent::CriticReturned {
                    critic_response: CriticResponse {
                        text: format!(
                            "critic reviewed '{}' for '{}'",
                            producer_response.text, task.name
                        ),
                    },
                }
            }

            DemoEffect::CallReferee {
                task,
                producer_response,
                critic_response,
            } => {
                sleep(Duration::from_secs(1));

                DemoEvent::RefereeReturned {
                    referee_response: RefereeResponse {
                        text: format!(
                            "referee accepted producer='{}', critic='{}' for '{}'",
                            producer_response.text, critic_response.text, task.name
                        ),
                    },
                }
            }
        }
    }

    /// Returns the final `TaskResult` once all three stages have completed.
    fn output(&self, state: &Self::State) -> Option<Self::Output> {
        match state {
            DemoState::PostReferee { result } => Some(result.clone()),
            _ => None,
        }
    }
}
