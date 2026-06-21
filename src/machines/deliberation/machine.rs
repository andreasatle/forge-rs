//! DeliberationMachine — transition logic and `Machine` implementation.
//!
//! Deliberation runs Producer → Critic → Referee before completing. When the
//! Referee rejects, the machine loops back to Producer with accumulated feedback,
//! up to `max_revisions` times. Final output is always the producer content;
//! critic and referee content do not replace it.
//!
//! ```text
//! Ready + Start
//!     → Waiting(Producer, revision_count=0, feedback=[])
//!     + RunRole(Producer, feedback=[])
//!
//! Waiting(Producer) + RoleReturned(Producer, Accepted { content })
//!     → Waiting(Critic, producer_content=Some(content))
//!     + RunRole(Critic, producer_content=Some(content))
//!
//! Waiting(Producer) + RoleReturned(Producer, Rejected { reason })
//!     → Failed + ReturnFailed
//!
//! Waiting(Critic, Some(pc)) + RoleReturned(Critic, Accepted { content })
//!     → Waiting(Referee, producer_content=Some(pc), critic_content=Some(content))
//!     + RunRole(Referee, …)
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
//! Waiting(Referee, …) + RoleReturned(Referee, Rejected { reason })
//!     revision_count < max_revisions:
//!         → Waiting(Producer, revision_count+1, feedback+[reason])
//!         + RunRole(Producer, feedback+[reason])
//!     revision_count >= max_revisions:
//!         → Failed("revision limit exhausted") + ReturnFailed
//!
//! Waiting(Referee, None, _) or Waiting(Referee, _, None) + RoleReturned(…)
//!     → Failed + ReturnFailed  (invalid deliberation state)
//!
//! Role mismatches → Failed + ReturnFailed (protocol violation)
//! ```

use crate::engine::{Machine, Transition};

use super::effect::DeliberationEffect;
use super::event::{DeliberationEvent, RoleResult};
use super::state::{DeliberationOutput, DeliberationRole, DeliberationState, RevisionFeedback};

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
                    feedback: vec![],
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    producer_content: None,
                    critic_content: None,
                    revision_count: 0,
                    feedback: vec![],
                },
            },

            // Producer accepted → hand off to Critic.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    revision_count,
                    feedback,
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
                    feedback: feedback.clone(),
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Critic,
                    producer_content: Some(content),
                    critic_content: None,
                    revision_count,
                    feedback,
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
                    revision_count,
                    feedback,
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
                    feedback: feedback.clone(),
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Referee,
                    producer_content: Some(producer_content),
                    critic_content: Some(critic_content),
                    revision_count,
                    feedback,
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

            // Referee rejected → loop back to Producer if revisions remain, otherwise fail.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Referee,
                    producer_content: Some(_),
                    critic_content: Some(_),
                    revision_count,
                    feedback,
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Referee,
                    result: RoleResult::Rejected { reason },
                },
            ) => {
                if revision_count < request.max_revisions {
                    let mut new_feedback = feedback;
                    new_feedback.push(RevisionFeedback {
                        reason: reason.clone(),
                    });
                    Transition {
                        effects: vec![DeliberationEffect::RunRole {
                            role: DeliberationRole::Producer,
                            objective: request.objective.clone(),
                            producer_content: None,
                            critic_content: None,
                            feedback: new_feedback.clone(),
                        }],
                        state: DeliberationState::Waiting {
                            request,
                            role: DeliberationRole::Producer,
                            producer_content: None,
                            critic_content: None,
                            revision_count: revision_count + 1,
                            feedback: new_feedback,
                        },
                    }
                } else {
                    let fail_reason = format!("revision limit exhausted: {reason}");
                    Transition {
                        effects: vec![DeliberationEffect::ReturnFailed {
                            reason: fail_reason.clone(),
                        }],
                        state: DeliberationState::Failed {
                            reason: fail_reason,
                        },
                    }
                }
            }

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
    use super::super::state::{DeliberationRequest, RevisionFeedback};
    use super::*;
    use crate::engine::run_machine;

    fn ready(objective: &str) -> DeliberationState {
        DeliberationState::Ready {
            request: DeliberationRequest {
                objective: objective.to_string(),
                max_revisions: 0,
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
                    revision_count: 0,
                    ..
                }
            ),
            "expected Waiting(Producer, None, None, revision_count=0), got {:?}",
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
                    feedback,
                } if objective == "write a poem" && feedback.is_empty()
            ),
            "expected RunRole(Producer, feedback=[]), got {:?}",
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
                max_revisions: 0,
            },
            role: DeliberationRole::Critic,
            producer_content: None,
            critic_content: None,
            revision_count: 0,
            feedback: vec![],
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
    fn referee_rejection_fails_when_no_revisions_allowed() {
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
            matches!(&t.state, DeliberationState::Failed { reason } if reason.contains("revision limit exhausted")),
            "expected Failed with 'revision limit exhausted', got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason } if reason.contains("revision limit exhausted")),
            "expected ReturnFailed with 'revision limit exhausted', got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn referee_missing_critic_content_fails() {
        let invalid_state = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
                max_revisions: 0,
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: None,
            revision_count: 0,
            feedback: vec![],
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
                max_revisions: 0,
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: Some("looks good".to_string()),
            revision_count: 0,
            feedback: vec![],
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

    #[test]
    fn referee_rejection_loops_to_producer_with_feedback() {
        let waiting_referee = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
                max_revisions: 1,
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: Some("review".to_string()),
            revision_count: 0,
            feedback: vec![],
        };

        let t = step(
            waiting_referee,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Referee,
                result: RoleResult::Rejected {
                    reason: "needs changes".to_string(),
                },
            },
        );

        // State must be Waiting(Producer) with revision_count=1 and feedback populated.
        match &t.state {
            DeliberationState::Waiting {
                role: DeliberationRole::Producer,
                revision_count,
                feedback,
                producer_content,
                critic_content,
                ..
            } => {
                assert_eq!(*revision_count, 1, "revision_count should be 1");
                assert_eq!(feedback.len(), 1, "feedback should have one entry");
                assert_eq!(
                    feedback[0].reason, "needs changes",
                    "feedback reason mismatch"
                );
                assert!(
                    producer_content.is_none(),
                    "producer_content should be None"
                );
                assert!(critic_content.is_none(), "critic_content should be None");
            }
            other => panic!("expected Waiting(Producer), got {:?}", other),
        }

        // Effect must be RunRole(Producer) with the same feedback.
        assert_eq!(t.effects.len(), 1);
        match &t.effects[0] {
            DeliberationEffect::RunRole {
                role: DeliberationRole::Producer,
                feedback,
                producer_content,
                critic_content,
                ..
            } => {
                assert_eq!(feedback.len(), 1);
                assert_eq!(feedback[0].reason, "needs changes");
                assert!(producer_content.is_none());
                assert!(critic_content.is_none());
            }
            other => panic!("expected RunRole(Producer), got {:?}", other),
        }
    }

    #[test]
    fn referee_rejection_exhausts_revision_limit() {
        let waiting_referee = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
                max_revisions: 1,
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: Some("review".to_string()),
            revision_count: 1, // already at the limit
            feedback: vec![RevisionFeedback {
                reason: "earlier rejection".to_string(),
            }],
        };

        let t = step(
            waiting_referee,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Referee,
                result: RoleResult::Rejected {
                    reason: "still not good enough".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { reason } if reason.contains("revision limit exhausted")),
            "expected Failed with 'revision limit exhausted', got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason } if reason.contains("revision limit exhausted")),
            "expected ReturnFailed with 'revision limit exhausted', got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn max_revisions_zero_fails_on_first_referee_rejection() {
        let waiting_referee = DeliberationState::Waiting {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
                max_revisions: 0,
            },
            role: DeliberationRole::Referee,
            producer_content: Some("draft".to_string()),
            critic_content: Some("review".to_string()),
            revision_count: 0,
            feedback: vec![],
        };

        let t = step(
            waiting_referee,
            DeliberationEvent::RoleReturned {
                role: DeliberationRole::Referee,
                result: RoleResult::Rejected {
                    reason: "not good".to_string(),
                },
            },
        );

        assert!(
            matches!(&t.state, DeliberationState::Failed { reason } if reason.contains("revision limit exhausted")),
            "expected Failed with 'revision limit exhausted', got {:?}",
            t.state
        );

        assert_eq!(t.effects.len(), 1);
        assert!(
            matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason } if reason.contains("revision limit exhausted")),
            "expected ReturnFailed, got {:?}",
            t.effects[0]
        );
    }

    #[test]
    fn revision_then_acceptance_completes_with_revised_producer_content() {
        struct FakeMachine {
            producer_call: std::cell::Cell<usize>,
        }

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
                    } => {
                        let call = self.producer_call.get();
                        self.producer_call.set(call + 1);
                        let content = if call == 0 {
                            "draft v1".to_string()
                        } else {
                            "draft v2".to_string()
                        };
                        DeliberationEvent::RoleReturned {
                            role: DeliberationRole::Producer,
                            result: RoleResult::Accepted { content },
                        }
                    }
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Critic,
                        ..
                    } => DeliberationEvent::RoleReturned {
                        role: DeliberationRole::Critic,
                        result: RoleResult::Accepted {
                            content: "looks fine".to_string(),
                        },
                    },
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Referee,
                        producer_content: Some(ref pc),
                        ..
                    } => {
                        if pc == "draft v1" {
                            DeliberationEvent::RoleReturned {
                                role: DeliberationRole::Referee,
                                result: RoleResult::Rejected {
                                    reason: "needs changes".to_string(),
                                },
                            }
                        } else {
                            DeliberationEvent::RoleReturned {
                                role: DeliberationRole::Referee,
                                result: RoleResult::Accepted {
                                    content: "approved".to_string(),
                                },
                            }
                        }
                    }
                    DeliberationEffect::RunRole { .. } => {
                        panic!("unexpected RunRole variant")
                    }
                    DeliberationEffect::ReturnComplete { .. } => {
                        unreachable!("ReturnComplete should not re-enter the loop")
                    }
                    other => panic!("unexpected effect: {:?}", other),
                }
            }

            fn output(&self, state: &DeliberationState) -> Option<DeliberationOutput> {
                DeliberationMachine.output(state)
            }
        }

        let initial = DeliberationState::Ready {
            request: DeliberationRequest {
                objective: "write a poem".to_string(),
                max_revisions: 1,
            },
        };

        let fake = FakeMachine {
            producer_call: std::cell::Cell::new(0),
        };
        let output = run_machine(fake, initial);
        assert_eq!(
            output.content, "draft v2",
            "final output should be revised producer content"
        );
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
                max_revisions: 0,
            },
        };

        let output = run_machine(FakeMachine, initial);
        assert_eq!(output.content, "draft");
    }
}
