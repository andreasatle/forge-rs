use std::thread::sleep;
use std::time::Duration;

use crate::engine::{Machine, Transition};

use super::effect::DemoEffect;
use super::event::DemoEvent;
use super::state::{
    CriticResponse, DemoState, ProducerResponse, RefereeResponse, TaskResult,
};

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
            (DemoState::NotStarted { task }, DemoEvent::Start) => Transition {
                state: DemoState::NotStarted { task: task.clone() },
                effects: vec![DemoEffect::CallProducer { task }],
            },

            (
                DemoState::NotStarted { task },
                DemoEvent::ProducerReturned { producer_response },
            ) => Transition {
                state: DemoState::PostProducer {
                    task: task.clone(),
                    producer_response: producer_response.clone(),
                },
                effects: vec![DemoEffect::CallCritic {
                    task,
                    producer_response,
                }],
            },

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

    fn output(&self, state: &Self::State) -> Option<Self::Output> {
        match state {
            DemoState::PostReferee { result } => Some(result.clone()),
            _ => None,
        }
    }
}
