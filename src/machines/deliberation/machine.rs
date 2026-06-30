//! DeliberationMachine — transition logic and `Machine` implementation.
//!
//! Deliberation runs Producer → Critic → Referee before completing. When the
//! Referee rejects, the machine loops back to Producer with accumulated feedback,
//! up to `max_revisions` times. Final output is always the producer content;
//! critic and referee content do not replace it.
//!
//! The Critic is advisory. The Referee is authoritative. Critic rejection is not
//! terminal — it routes to the Referee, which makes the final accept/reject
//! decision. Only the Referee controls revision.
//!
//! ```text
//! Ready + Start
//!     → Waiting(Producer, feedback=[])
//!     + RunRole(Producer, feedback=[])
//!
//! Waiting(Producer) + ProducerAccepted { content, artifact_changed }
//!     → Waiting(Producer, producer_content=Some(content))
//!     + ValidateProducer(content, artifact_changed)
//!
//! Waiting(Producer, producer_content=Some(content))
//!     + ProducerValidationReturned(Valid)
//!     → Waiting(Critic, producer_content=Some(content))
//!     + RunRole(Critic, producer_content=Some(content))
//!
//! Waiting(Producer, producer_content=Some(content))
//!     + ProducerValidationReturned(Retry)
//!     validation_attempt < max_validation_retries:
//!         → Waiting(Producer, validation_attempt+1, validation_feedback=[reason])
//!         + RunRole(Producer, feedback=[reason])
//!     validation_attempt >= max_validation_retries:
//!         → Failed
//!
//! Waiting(Producer) + RoleReturned(Producer, Rejected { reason })
//!     → Failed
//!
//! Waiting(Producer) + RoleReturned(Producer, Failed { reason })
//!     → Failed  (execution failure, not semantic rejection)
//!
//! Waiting(Critic, Some(pc)) + RoleReturned(Critic, Accepted { content })
//!     → Waiting(Referee, producer_content=Some(pc), critic_advisory=AcceptedReview)
//!     + RunRole(Referee, …)
//!
//! Waiting(Critic, Some(pc)) + RoleReturned(Critic, Rejected { reason })
//!     → Waiting(Referee, producer_content=Some(pc), critic_advisory=RejectedReason)
//!     + RunRole(Referee, producer_content=Some(pc), critic_content=Some(reason))
//!     (Critic is advisory; Referee decides)
//!
//! Waiting(Critic, Some(_)) + RoleReturned(Critic, Failed { reason })
//!     → Failed  (execution failure, not semantic rejection)
//!
//! Waiting(Critic, None) + RoleReturned(Critic, …)
//!     → Failed  (invalid deliberation state)
//!
//! Waiting(Referee, Some(pc), Some(_)) + RoleReturned(Referee, Accepted)
//!     → Complete { output: pc }   ← output is producer content
//!
//! Waiting(Referee, …) + RoleReturned(Referee, Rejected { reason })
//!     feedback.len() < max_revisions:
//!         → Waiting(Producer, feedback+[reason])
//!         + RunRole(Producer, feedback+[reason])
//!     feedback.len() >= max_revisions:
//!         → Failed(reason=RevisionLimitExhausted)
//!
//! Waiting(Referee, Some(_), Some(_)) + RoleReturned(Referee, Failed { reason })
//!     → Failed  (execution failure — must NOT enter the revision loop)
//!
//! Waiting(Referee, None, _) or Waiting(Referee, _, None advisory) + RoleReturned(…)
//!     → Failed  (invalid deliberation state)
//!
//! Role mismatches → Failed (protocol violation)
//! ```

use crate::engine::{Machine, Transition};
use crate::machines::scheduler::FailureKind;

use super::effect::DeliberationEffect;
use super::event::{DeliberationEvent, ProducerValidationResult, RoleResult};
use super::state::{
    CriticAdvisory, DeliberationFailureReason, DeliberationOutput, DeliberationRole,
    DeliberationState, DeliberationTerminalOutput, ProducerValidationState, RevisionFeedback,
};

/// The deliberation machine. All durable data travels in `DeliberationState`.
pub struct DeliberationMachine;

impl DeliberationMachine {
    fn failed_transition(
        kind: FailureKind,
        reason: DeliberationFailureReason,
        message: String,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        Transition {
            effects: vec![],
            state: DeliberationState::Failed {
                kind,
                reason,
                message,
            },
        }
    }

    fn producer_accepted_transition(
        request: super::state::DeliberationRequest,
        feedback: Vec<RevisionFeedback>,
        producer_validation: ProducerValidationState,
        content: String,
        artifact_changed: bool,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        Transition {
            effects: vec![DeliberationEffect::ValidateProducer {
                content: content.clone(),
                artifact_changed,
            }],
            state: DeliberationState::Waiting {
                request,
                role: DeliberationRole::Producer,
                producer_content: Some(content),
                critic_advisory: None,
                feedback,
                producer_validation,
            },
        }
    }
}

impl Machine for DeliberationMachine {
    type State = DeliberationState;
    type Event = DeliberationEvent;
    type Effect = DeliberationEffect;
    type Output = DeliberationTerminalOutput;

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
                    context: request.context.clone(),
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    producer_content: None,
                    critic_advisory: None,
                    feedback: vec![],
                    producer_validation: ProducerValidationState {
                        attempt: 0,
                        feedback: vec![],
                    },
                },
            },

            // Producer accepted with artifact metadata from the handler.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    feedback,
                    producer_validation,
                    ..
                },
                DeliberationEvent::ProducerAccepted {
                    content,
                    artifact_changed,
                },
            ) => Self::producer_accepted_transition(
                request,
                feedback,
                producer_validation,
                content,
                artifact_changed,
            ),

            // Producer validation accepted → hand off to Critic.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    producer_content: Some(producer_content),
                    feedback,
                    ..
                },
                DeliberationEvent::ProducerValidationReturned {
                    content,
                    result: ProducerValidationResult::Valid,
                },
            ) if producer_content == content => Transition {
                effects: vec![DeliberationEffect::RunRole {
                    role: DeliberationRole::Critic,
                    objective: request.objective.clone(),
                    context: request.context.clone(),
                    producer_content: Some(producer_content.clone()),
                    critic_content: None,
                    feedback: feedback.clone(),
                }],
                state: DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Critic,
                    producer_content: Some(producer_content),
                    critic_advisory: None,
                    feedback,
                    producer_validation: ProducerValidationState {
                        attempt: 0,
                        feedback: vec![],
                    },
                },
            },

            // Producer validation rejected → retry Producer if validation retry budget remains.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Producer,
                    producer_content: Some(producer_content),
                    feedback,
                    producer_validation,
                    ..
                },
                DeliberationEvent::ProducerValidationReturned {
                    content,
                    result:
                        ProducerValidationResult::Retry {
                            feedback_reason,
                            max_retries,
                            failure_kind,
                            failure_reason,
                        },
                },
            ) if producer_content == content => {
                if producer_validation.attempt < max_retries {
                    let validation_feedback = vec![RevisionFeedback {
                        reason: feedback_reason,
                    }];
                    Transition {
                        effects: vec![DeliberationEffect::RunRole {
                            role: DeliberationRole::Producer,
                            objective: request.objective.clone(),
                            context: request.context.clone(),
                            producer_content: None,
                            critic_content: None,
                            feedback: validation_feedback.clone(),
                        }],
                        state: DeliberationState::Waiting {
                            request,
                            role: DeliberationRole::Producer,
                            producer_content: None,
                            critic_advisory: None,
                            feedback,
                            producer_validation: ProducerValidationState {
                                attempt: producer_validation.attempt + 1,
                                feedback: validation_feedback,
                            },
                        },
                    }
                } else {
                    Self::failed_transition(
                        failure_kind,
                        DeliberationFailureReason::ProducerValidationRetriesExhausted,
                        failure_reason,
                    )
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
            ) => Self::failed_transition(
                FailureKind::UserTaskRejection,
                DeliberationFailureReason::ProducerRejected,
                reason,
            ),

            // Producer execution failure → terminal.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Producer,
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Failed { kind, reason },
                },
            ) => Self::failed_transition(
                kind,
                DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Producer,
                },
                reason,
            ),

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
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
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
                Self::failed_transition(
                    FailureKind::DeliberationFailure,
                    DeliberationFailureReason::InvalidState,
                    reason,
                )
            }

            // Critic accepted → hand off to Referee.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Critic,
                    producer_content: Some(producer_content),
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
            ) => {
                let critic_advisory = CriticAdvisory::AcceptedReview {
                    content: critic_content.clone(),
                };
                Transition {
                    effects: vec![DeliberationEffect::RunRole {
                        role: DeliberationRole::Referee,
                        objective: request.objective.clone(),
                        context: request.context.clone(),
                        producer_content: Some(producer_content.clone()),
                        critic_content: Some(critic_advisory.as_referee_content().to_string()),
                        feedback: feedback.clone(),
                    }],
                    state: DeliberationState::Waiting {
                        request,
                        role: DeliberationRole::Referee,
                        producer_content: Some(producer_content),
                        critic_advisory: Some(critic_advisory),
                        feedback,
                        producer_validation: ProducerValidationState {
                            attempt: 0,
                            feedback: vec![],
                        },
                    },
                }
            }

            // Critic rejected → route to Referee with a typed advisory reason.
            // The Critic is advisory; only the Referee is authoritative.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Critic,
                    producer_content: Some(producer_content),
                    feedback,
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Critic,
                    result: RoleResult::Rejected { reason },
                },
            ) => {
                let critic_advisory = CriticAdvisory::RejectedReason {
                    reason: reason.clone(),
                };
                Transition {
                    effects: vec![DeliberationEffect::RunRole {
                        role: DeliberationRole::Referee,
                        objective: request.objective.clone(),
                        context: request.context.clone(),
                        producer_content: Some(producer_content.clone()),
                        critic_content: Some(critic_advisory.as_referee_content().to_string()),
                        feedback: feedback.clone(),
                    }],
                    state: DeliberationState::Waiting {
                        request,
                        role: DeliberationRole::Referee,
                        producer_content: Some(producer_content),
                        critic_advisory: Some(critic_advisory),
                        feedback,
                        producer_validation: ProducerValidationState {
                            attempt: 0,
                            feedback: vec![],
                        },
                    },
                }
            }

            // Critic execution failure → terminal.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Critic,
                    producer_content: Some(_),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Critic,
                    result: RoleResult::Failed { kind, reason },
                },
            ) => Self::failed_transition(
                kind,
                DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Critic,
                },
                reason,
            ),

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
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            // Referee returned but producer_content or critic_advisory is missing — invalid state.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content,
                    critic_advisory,
                    ..
                },
                DeliberationEvent::RoleReturned { .. },
            ) if producer_content.is_none() || critic_advisory.is_none() => {
                let reason =
                    "invalid deliberation state: Referee returned but producer_content or critic_advisory is missing"
                        .to_string();
                Self::failed_transition(
                    FailureKind::DeliberationFailure,
                    DeliberationFailureReason::InvalidState,
                    reason,
                )
            }

            // Referee accepted → complete with producer content (not referee content).
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content: Some(producer_content),
                    critic_advisory: Some(_),
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
                    effects: vec![],
                    state: DeliberationState::Complete { output },
                }
            }

            // Referee rejected → loop back to Producer if revisions remain, otherwise fail.
            (
                DeliberationState::Waiting {
                    request,
                    role: DeliberationRole::Referee,
                    producer_content: Some(_),
                    critic_advisory: Some(_),
                    feedback,
                    producer_validation: _,
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Referee,
                    result: RoleResult::Rejected { reason },
                },
            ) => {
                if feedback.len() < request.max_revisions {
                    let mut new_feedback = feedback;
                    new_feedback.push(RevisionFeedback {
                        reason: reason.clone(),
                    });
                    Transition {
                        effects: vec![DeliberationEffect::RunRole {
                            role: DeliberationRole::Producer,
                            objective: request.objective.clone(),
                            context: request.context.clone(),
                            producer_content: None,
                            critic_content: None,
                            feedback: new_feedback.clone(),
                        }],
                        state: DeliberationState::Waiting {
                            request,
                            role: DeliberationRole::Producer,
                            producer_content: None,
                            critic_advisory: None,
                            feedback: new_feedback,
                            producer_validation: ProducerValidationState {
                                attempt: 0,
                                feedback: vec![],
                            },
                        },
                    }
                } else {
                    let fail_reason = format!("revision limit exhausted: {reason}");
                    Self::failed_transition(
                        FailureKind::DeliberationFailure,
                        DeliberationFailureReason::RevisionLimitExhausted,
                        fail_reason,
                    )
                }
            }

            // Referee execution failure → terminal. Must NOT enter the revision loop.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content: Some(_),
                    critic_advisory: Some(_),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Referee,
                    result: RoleResult::Failed { kind, reason },
                },
            ) => Self::failed_transition(
                kind,
                DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Referee,
                },
                reason,
            ),

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
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            (state, event) => {
                let reason = format!("invalid transition: state={state:?}, event={event:?}");
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::InvalidTransition,
                    reason,
                )
            }
        }
    }

    fn handle_effect(&self, _effect: Self::Effect) -> Self::Event {
        panic!("handle_effect called on bare DeliberationMachine — use a test wrapper")
    }

    fn output(&self, state: &Self::State) -> Option<Self::Output> {
        match state {
            DeliberationState::Complete { output } => {
                Some(DeliberationTerminalOutput::Complete(output.clone()))
            }
            DeliberationState::Failed {
                kind,
                reason,
                message,
            } => Some(DeliberationTerminalOutput::Failed {
                kind: *kind,
                reason: reason.clone(),
                message: message.clone(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "machine_tests/mod.rs"]
mod machine_tests;
