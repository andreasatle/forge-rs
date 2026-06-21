//! DeliberationMachine — transition logic and `Machine` implementation.
//!
//! Phase 1 wires only the Producer role:
//!
//! ```text
//! Ready + Start → Waiting(Producer) + RunRole(Producer)
//! Waiting(Producer) + RoleReturned(Producer, Accepted) → Complete + ReturnComplete
//! Waiting(Producer) + RoleReturned(Producer, Rejected) → Failed  + ReturnFailed
//! Waiting(Producer) + RoleReturned(Critic|Referee, …)  → Failed  + ReturnFailed (protocol violation)
//! ```
//!
//! Critic and Referee transitions will be added in later phases. Unimplemented
//! roles fail clearly rather than panic.

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
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                },
            },

            // Producer accepted → complete.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Producer,
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Accepted { content },
                },
            ) => {
                let output = DeliberationOutput { content };
                Transition {
                    effects: vec![DeliberationEffect::ReturnComplete {
                        output: output.clone(),
                    }],
                    state: DeliberationState::Complete { output },
                }
            }

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

            // Unimplemented: Critic or Referee dispatched.
            (DeliberationState::Waiting { role, .. }, DeliberationEvent::RoleReturned { .. }) => {
                let reason = format!(
                    "protocol violation: role {:?} is not yet implemented in Phase 1",
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
        // No real I/O in Phase 1; this method is only exercised in tests via a
        // wrapper or the smoke-test fake handler below.
        panic!("handle_effect called on bare DeliberationMachine — use a test wrapper")
    }

    fn output(&self, state: &Self::State) -> Option<Self::Output> {
        match state {
            DeliberationState::Complete { output } => Some(output.clone()),
            DeliberationState::Failed { .. } => {
                // Terminal but no value — the runner will loop forever if we
                // return None here, so we treat Failed as unreachable after
                // ReturnFailed is handled. In the smoke test the fake handler
                // never produces a rejected result.
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::run_machine;

    fn ready(objective: &str) -> DeliberationState {
        DeliberationState::Ready {
            request: super::super::state::DeliberationRequest {
                objective: objective.to_string(),
            },
        }
    }

    fn machine() -> DeliberationMachine {
        DeliberationMachine
    }

    // Helper: call transition directly without the runner.
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
                    ..
                }
            ),
            "expected Waiting(Producer), got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(
                &t.effects[0],
                DeliberationEffect::RunRole {
                    role: DeliberationRole::Producer,
                    objective,
                } if objective == "write a poem"
            ),
            "expected RunRole(Producer), got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn producer_acceptance_completes() {
        let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

        let t = step(
            waiting,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Producer,
                result: RoleResult::Accepted {
                    content: "roses are red".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Complete { output } if output.content == "roses are red"),
            "expected Complete with matching content, got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(
                &t.effects[0],
                DeliberationEffect::ReturnComplete { output } if output.content == "roses are red"
            ),
            "expected ReturnComplete, got {:?}",
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
    fn role_mismatch_fails() {
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

        assert_eq!(t.effects.len(), 1);
        let reason = match &t.effects[0] {
            DeliberationEffect::ReturnFailed { reason } => reason,
            other => panic!("expected ReturnFailed, got {:?}", other),
        };
        assert!(
            reason.contains("protocol violation"),
            "expected reason to contain 'protocol violation', got: {reason}"
        );
    }

    // Smoke test using the real runner with a fake handler that simulates
    // a Producer acceptance.
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
                        objective: _,
                    } => DeliberationEvent::RoleReturned {
                        role: DeliberationRole::Producer,
                        result: RoleResult::Accepted {
                            content: "fake producer output".to_string(),
                        },
                    },
                    DeliberationEffect::ReturnComplete { .. } => {
                        // Terminal effect — runner won't call this after output() fires.
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
            request: super::super::state::DeliberationRequest {
                objective: "smoke test".to_string(),
            },
        };

        let output = run_machine(FakeMachine, initial);
        assert_eq!(output.content, "fake producer output");
    }
}
