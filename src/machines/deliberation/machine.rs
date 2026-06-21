//! DeliberationMachine — transition logic and `Machine` implementation.
//!
//! Deliberation runs Producer → Critic → Referee before completing. Final output
//! is always the producer content; critic and referee content do not replace it.
//! Revision loops are not yet implemented.
//!
//! ```text
//! Ready + Start
//!     → Waiting(Producer, producer_content=None, critic_content=None)
//!     + RunRole(Producer, producer_content=None, critic_content=None)
//!
//! Waiting(Producer) + RoleReturned(Producer, Accepted { content })
//!     → Waiting(Critic, producer_content=Some(content), critic_content=None)
//!     + RunRole(Critic, producer_content=Some(content), critic_content=None)
//!
//! Waiting(Producer) + RoleReturned(Producer, Rejected { reason })
//!     → Failed + ReturnFailed
//!
//! Waiting(Critic, Some(pc)) + RoleReturned(Critic, Accepted { content })
//!     → Waiting(Referee, producer_content=Some(pc), critic_content=Some(content))
//!     + RunRole(Referee, producer_content=Some(pc), critic_content=Some(content))
//!
//! Waiting(Critic, Some(_)) + RoleReturned(Critic, Rejected { reason })
//!     → Failed + ReturnFailed
//!
//! Waiting(Critic, None) + RoleReturned(Critic, …)
//!     → Failed + ReturnFailed  (invalid deliberation state)
//!
//! Waiting(Referee, Some(pc), Some(_)) + RoleReturned(Referee, Accepted)
//!     → Complete { output: pc } + ReturnComplete   ← output is producer content
//!
//! Waiting(Referee, Some(_), Some(_)) + RoleReturned(Referee, Rejected { reason })
//!     → Failed + ReturnFailed
//!
//! Waiting(Referee, None, _) or Waiting(Referee, _, None) + RoleReturned(…)
//!     → Failed + ReturnFailed  (invalid deliberation state)
//!
//! Role mismatches → Failed + ReturnFailed (protocol violation)
//! ```

use crate::engine::{Machine, Transition};

use super::effect::DeliberationEffect;
use super::event::{DeliberationEvent, RoleResult};
use super::state::{DeliberationOutput, DeliberationRole, DeliberationState};

/// The deliberation machine. All durable data travels in `DeliberationState`.
pub struct DeliberationMachine;

impl Machine for DeliberationMachine {
    type State = DeliberationState;
    type Event = DeliberationEvent;
    type Effect = DeliberationEffect;
    type Output = DeliberationOutput;

    fn start_event(&self) -> Self::Event {
        DeliberationEvent::Start
    }

    fn transition(
        &self,
        state: Self::State,
        event: Self::Event,
    ) -> Transition<Self::State, Self::Effect> {
        match (state, event) {
            // Bootstrap: kick off the Producer.
            (DeliberationState::Ready { request }, DeliberationEvent::Start) => Transition {
                effects: vec![DeliberationEffect::RunRole {
                    role: DeliberationRole::Producer,
                    objective: request.objective.clone(),
                    producer_content: None,
                    critic_content: None,
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    producer_content: None,
                    critic_content: None,
                },
            },

            // Producer accepted → hand off to Critic.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Accepted { content },
                },
            ) => Transition {
                effects: vec![DeliberationEffect::RunRole {
                    role: DeliberationRole::Critic,
                    objective: request.objective.clone(),
                    producer_content: Some(content.clone()),
                    critic_content: None,
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Critic,
                    producer_content: Some(content),
                    critic_content: None,
                },
            },

            // Producer rejected → failed.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Producer,
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Rejected { reason },
                },
            ) => Transition {
                effects: vec![DeliberationEffect::ReturnFailed {
                    reason: reason.clone(),
                }],
                state: DeliberationState::Failed { reason },
            },

            // Role mismatch while waiting for Producer → protocol violation.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Producer,
                    ..
                },
                DeliberationEvent::RoleReturned { role, .. },
            ) => {
                let reason = format!(
                    "protocol violation: expected Producer result but received {:?}",
                    role
                );
                Transition {
                    effects: vec![DeliberationEffect::ReturnFailed {
                        reason: reason.clone(),
                    }],
                    state: DeliberationState::Failed { reason },
                }
            }

            // Critic returned but producer content is missing — invalid state.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Critic,
                    producer_content: None,
                    ..
                },
                DeliberationEvent::RoleReturned { .. },
            ) => {
                let reason =
                    "invalid deliberation state: Critic returned but producer_content is missing"
                        .to_string();
                Transition {
                    effects: vec![DeliberationEffect::ReturnFailed {
                        reason: reason.clone(),
                    }],
                    state: DeliberationState::Failed { reason },
                }
            }

            // Critic accepted → hand off to Referee.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Critic,
                    producer_content: Some(producer_content),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Critic,
                    result:
                        RoleResult::Accepted {
                            content: critic_content,
                        },
                },
            ) => Transition {
                effects: vec![DeliberationEffect::RunRole {
                    role: DeliberationRole::Referee,
                    objective: request.objective.clone(),
                    producer_content: Some(producer_content.clone()),
                    critic_content: Some(critic_content.clone()),
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Referee,
                    producer_content: Some(producer_content),
                    critic_content: Some(critic_content),
                },
            },

            // Critic rejected → failed.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Critic,
                    producer_content: Some(_),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Critic,
                    result: RoleResult::Rejected { reason },
                },
            ) => Transition {
                effects: vec![DeliberationEffect::ReturnFailed {
                    reason: reason.clone(),
                }],
                state: DeliberationState::Failed { reason },
            },

            // Role mismatch while waiting for Critic → protocol violation.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Critic,
                    ..
                },
                DeliberationEvent::RoleReturned { role, .. },
            ) => {
                let reason = format!(
                    "protocol violation: expected Critic result but received {:?}",
                    role
                );
                Transition {
                    effects: vec![DeliberationEffect::ReturnFailed {
                        reason: reason.clone(),
                    }],
                    state: DeliberationState::Failed { reason },
                }
            }

            // Referee returned but producer_content or critic_content is missing — invalid state.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content,
                    critic_content,
                    ..
                },
                DeliberationEvent::RoleReturned { .. },
            ) if producer_content.is_none() || critic_content.is_none() => {
                let reason =
                    "invalid deliberation state: Referee returned but producer_content or critic_content is missing"
                        .to_string();
                Transition {
                    effects: vec![DeliberationEffect::ReturnFailed {
                        reason: reason.clone(),
                    }],
                    state: DeliberationState::Failed { reason },
                }
            }

            // Referee accepted → complete with producer content (not referee content).
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content: Some(producer_content),
                    critic_content: Some(_),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Referee,
                    result: RoleResult::Accepted { .. },
                },
            ) => {
                let output = DeliberationOutput {
                    content: producer_content,
                };
                Transition {
                    effects: vec![DeliberationEffect::ReturnComplete {
                        output: output.clone(),
                    }],
                    state: DeliberationState::Complete { output },
                }
            }

            // Referee rejected → failed.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content: Some(_),
                    critic_content: Some(_),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Referee,
                    result: RoleResult::Rejected { reason },
                },
            ) => Transition {
                effects: vec![DeliberationEffect::ReturnFailed {
                    reason: reason.clone(),
                }],
                state: DeliberationState::Failed { reason },
            },

            // Role mismatch while waiting for Referee → protocol violation.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    ..
                },
                DeliberationEvent::RoleReturned { role, .. },
            ) => {
                let reason = format!(
                    "protocol violation: expected Referee result but received {:?}",
                    role
                );
                Transition {
                    effects: vec![DeliberationEffect::ReturnFailed {
                        reason: reason.clone(),
                    }],
                    state: DeliberationState::Failed { reason },
                }
            }

            (state, event) => {
                let reason = format!("invalid transition: state={state:?}, event={event:?}");
                Transition {
                    effects: vec![DeliberationEffect::ReturnFailed {
                        reason: reason.clone(),
                    }],
                    state: DeliberationState::Failed { reason },
                }
            }
        }
    }

    fn handle_effect(&self, _effect: Self::Effect) -> Self::Event {
        panic!("handle_effect called on bare DeliberationMachine — use a test wrapper")
    }

    fn output(&self, state: &Self::State) -> Option<Self::Output> {
        match state {
            DeliberationState::Complete { output } => Some(output.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::DeliberationRequest;
    use super::*;
    use crate::engine::run_machine;

    fn ready(objective: &str) -> DeliberationState {
        DeliberationState::Ready {
            request: DeliberationRequest {
                objective: objective.to_string(),
            },
        }
    }

    fn machine() -> DeliberationMachine {
        DeliberationMachine
    }

    fn step(
        state: DeliberationState,
        event: DeliberationEvent,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        machine().transition(state, event)
    }

    #[test]
    fn ready_start_runs_producer() {
        let t = step(ready("write a poem"), DeliberationEvent::Start);

        assert!(
            matches!(
                &t.state,
                DeliberationState::Waiting {
                    role: DeliberationRole::Producer,
                    producer_content: None,
                    critic_content: None,
                    ..
                }
            ),
            "expected Waiting(Producer, None, None), got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(
                &t.effects[0],
                DeliberationEffect::RunRole {
                    role: DeliberationRole::Producer,
                    objective,
                    producer_content: None,
                    critic_content: None,
                } if objective == "write a poem"
            ),
            "expected RunRole(Producer, producer_content=None, critic_content=None), got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn producer_acceptance_runs_critic() {
        let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

        let t = step(
            waiting,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Accepted {
                    content: "draft content".to_string(),
                },
            },
        );

        assert!(
            matches!(
                &t.state,
                DeliberationState::Waiting {
                    role: DeliberationRole::Critic,
                    producer_content: Some(pc),
                    critic_content: None,
                    ..
                } if pc == "draft content"
            ),
            "expected Waiting(Critic, Some('draft content'), None), got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(
                &t.effects[0],
                DeliberationEffect::RunRole {
                    role: DeliberationRole::Critic,
                    producer_content: Some(pc),
                    critic_content: None,
                    ..
                } if pc == "draft content"
            ),
            "expected RunRole(Critic, producer_content=Some('draft content'), critic_content=None), got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn producer_rejection_fails() {
        let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

        let t = step(
            waiting,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Rejected {
                    reason: "out of ideas".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { reason } if reason == "out of ideas"),
            "expected Failed, got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason } if reason == "out of ideas"),
            "expected ReturnFailed, got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn role_mismatch_while_waiting_producer_fails() {
        let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

        let t = step(
            waiting,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Critic,
                result: RoleResult::Accepted {
                    content: "unexpected".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { .. }),
            "expected Failed, got {:?}",
            t.state
        );

        let reason = match &t.effects[0] {
            DeliberationEffect::ReturnFailed { reason } => reason,
            other => panic!("expected ReturnFailed, got {:?}", other),
        };
        assert!(
            reason.contains("protocol violation"),
            "expected 'protocol violation' in reason, got: {reason}"
        );
    }

    #[test]
    fn critic_acceptance_runs_referee() {
        let after_producer = step(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Accepted {
                    content: "draft content".to_string(),
                },
            },
        )
        .state;

        let t = step(
            after_producer,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Critic,
                result: RoleResult::Accepted {
                    content: "looks good".to_string(),
                },
            },
        );

        // Must not complete yet — should enter Waiting(Referee).
        assert!(
            matches!(
                &t.state,
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content: Some(pc),
                    critic_content: Some(cc),
                    ..
                } if pc == "draft content" && cc == "looks good"
            ),
            "expected Waiting(Referee, Some('draft content'), Some('looks good')), got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(
                &t.effects[0],
                DeliberationEffect::RunRole {
                    role: DeliberationRole::Referee,
                    producer_content: Some(pc),
                    critic_content: Some(cc),
                    ..
                } if pc == "draft content" && cc == "looks good"
            ),
            "expected RunRole(Referee, producer_content=Some('draft content'), critic_content=Some('looks good')), got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn critic_rejection_fails() {
        let after_producer = step(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Accepted {
                    content: "draft content".to_string(),
                },
            },
        )
        .state;

        let t = step(
            after_producer,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Critic,
                result: RoleResult::Rejected {
                    reason: "too short".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { reason } if reason == "too short"),
            "expected Failed, got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason } if reason == "too short"),
            "expected ReturnFailed, got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn critic_missing_producer_content_fails() {
        let invalid_state = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
            },
            role: DeliberationRole::Critic,
            producer_content: None,
            critic_content: None,
        };

        let t = step(
            invalid_state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Critic,
                result: RoleResult::Accepted {
                    content: "shouldn't matter".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { .. }),
            "expected Failed, got {:?}",
            t.state
        );

        let reason = match &t.effects[0] {
            DeliberationEffect::ReturnFailed { reason } => reason,
            other => panic!("expected ReturnFailed, got {:?}", other),
        };
        assert!(
            reason.contains("invalid deliberation state"),
            "expected 'invalid deliberation state' in reason, got: {reason}"
        );
    }

    #[test]
    fn role_mismatch_while_waiting_critic_fails() {
        let after_producer = step(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Accepted {
                    content: "draft".to_string(),
                },
            },
        )
        .state;

        let t = step(
            after_producer,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Accepted {
                    content: "wrong role".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { .. }),
            "expected Failed, got {:?}",
            t.state
        );

        let reason = match &t.effects[0] {
            DeliberationEffect::ReturnFailed { reason } => reason,
            other => panic!("expected ReturnFailed, got {:?}", other),
        };
        assert!(
            reason.contains("protocol violation"),
            "expected 'protocol violation' in reason, got: {reason}"
        );
    }

    #[test]
    fn referee_acceptance_completes_with_producer_content() {
        let after_critic = step(
            step(
                step(ready("write a poem"), DeliberationEvent::Start).state,
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Accepted {
                        content: "draft content".to_string(),
                    },
                },
            )
            .state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Critic,
                result: RoleResult::Accepted {
                    content: "looks good".to_string(),
                },
            },
        )
        .state;

        let t = step(
            after_critic,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Referee,
                result: RoleResult::Accepted {
                    content: "referee notes (ignored)".to_string(),
                },
            },
        );

        assert!(
            matches!(
                &t.state,
                DeliberationState::Complete { output } if output.content == "draft content"
            ),
            "expected Complete with producer content 'draft content', got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(
                &t.effects[0],
                DeliberationEffect::ReturnComplete { output } if output.content == "draft content"
            ),
            "expected ReturnComplete with producer content, got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn referee_rejection_fails() {
        let after_critic = step(
            step(
                step(ready("write a poem"), DeliberationEvent::Start).state,
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Accepted {
                        content: "draft content".to_string(),
                    },
                },
            )
            .state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Critic,
                result: RoleResult::Accepted {
                    content: "looks good".to_string(),
                },
            },
        )
        .state;

        let t = step(
            after_critic,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Referee,
                result: RoleResult::Rejected {
                    reason: "not acceptable".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { reason } if reason == "not acceptable"),
            "expected Failed, got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason } if reason == "not acceptable"),
            "expected ReturnFailed, got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn referee_missing_critic_content_fails() {
        let invalid_state = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: None,
        };

        let t = step(
            invalid_state,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Referee,
                result: RoleResult::Accepted {
                    content: "shouldn't matter".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { .. }),
            "expected Failed, got {:?}",
            t.state
        );

        let reason = match &t.effects[0] {
            DeliberationEffect::ReturnFailed { reason } => reason,
            other => panic!("expected ReturnFailed, got {:?}", other),
        };
        assert!(
            reason.contains("invalid deliberation state"),
            "expected 'invalid deliberation state' in reason, got: {reason}"
        );
    }

    #[test]
    fn role_mismatch_while_waiting_referee_fails() {
        let waiting_referee = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: Some("looks good".to_string()),
        };

        for wrong_role in [DeliberationRole::Producer, DeliberationRole::Critic] {
            let t = step(
                waiting_referee.clone(),
                DeliberationEvent::RoleReturned {
                    role: wrong_role,
                    result: RoleResult::Accepted {
                        content: "unexpected".to_string(),
                    },
                },
            );

            assert!(
                matches!(&t.state, DeliberationState::Failed { .. }),
                "expected Failed, got {:?}",
                t.state
            );

            let reason = match &t.effects[0] {
                DeliberationEffect::ReturnFailed { reason } => reason,
                other => panic!("expected ReturnFailed, got {:?}", other),
            };
            assert!(
                reason.contains("protocol violation"),
                "expected 'protocol violation' in reason, got: {reason}"
            );
        }
    }

    // Smoke test: Producer → Accepted("draft"), Critic → Accepted("looks good"),
    // Referee → Accepted("approved"). Final output must be "draft" (producer content).
    #[test]
    fn run_machine_deliberation_smoke_test() {
        struct FakeMachine;

        impl Machine for FakeMachine {
            type State = DeliberationState;
            type Event = DeliberationEvent;
            type Effect = DeliberationEffect;
            type Output = DeliberationOutput;

            fn start_event(&self) -> DeliberationEvent {
                DeliberationEvent::Start
            }

            fn transition(
                &self,
                state: DeliberationState,
                event: DeliberationEvent,
            ) -> Transition<DeliberationState, DeliberationEffect> {
                DeliberationMachine.transition(state, event)
            }

            fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
                match effect {
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Producer,
                        ..
                    } => DeliberationEvent::RoleReturned {
                        role: DeliberationRole::Producer,
                        result: RoleResult::Accepted {
                            content: "draft".to_string(),
                        },
                    },
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Critic,
                        ..
                    } => DeliberationEvent::RoleReturned {
                        role: DeliberationRole::Critic,
                        result: RoleResult::Accepted {
                            content: "looks good".to_string(),
                        },
                    },
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Referee,
                        ..
                    } => DeliberationEvent::RoleReturned {
                        role: DeliberationRole::Referee,
                        result: RoleResult::Accepted {
                            content: "approved".to_string(),
                        },
                    },
                    DeliberationEffect::ReturnComplete { .. } => {
                        unreachable!("ReturnComplete should not re-enter the loop")
                    }
                    other => panic!("unexpected effect in smoke test: {:?}", other),
                }
            }

            fn output(&self, state: &DeliberationState) -> Option<DeliberationOutput> {
                DeliberationMachine.output(state)
            }
        }

        let initial = DeliberationState::Ready {
            request: DeliberationRequest {
                objective: "smoke test".to_string(),
            },
        };

        let output = run_machine(FakeMachine, initial);
        assert_eq!(output.content, "draft");
    }
}
