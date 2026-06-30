//! DeliberationMachine — transition logic and `Machine` implementation.
//!
//! Deliberation runs Producer → Validator → Critic → Referee before completing.
//! When the Referee rejects, the machine loops back to Producer with accumulated
//! feedback, up to `max_revisions` times. Final output is always the producer
//! content; critic and referee content do not replace it.
//!
//! The Critic is advisory. The Referee is authoritative. Critic rejection is not
//! terminal — it routes to the Referee, which makes the final accept/reject
//! decision. Only the Referee controls revision.
//!
//! The transition function implements:
//!
//! ```text
//! (DeliberationState, DeliberationEvent) -> (DeliberationState, DeliberationEffect)
//! ```
//!
//! ```text
//! Ready + Start
//!     → WaitingProducer(feedback=[], validation_attempt=0)
//!     + RunRole(Producer, feedback=[])
//!
//! WaitingProducer + ProducerAccepted { content, artifact_changed }
//!     → WaitingValidator(producer_content=content, validation_attempt=N)
//!     + ValidateProducer(content, artifact_changed)
//!
//! WaitingValidator(producer_content=content, validation_attempt=N)
//!     + ProducerValidationAccepted
//!     → WaitingCritic(producer_content=content)
//!     + RunRole(Critic, producer_content=content)
//!
//! WaitingValidator(producer_content=content, validation_attempt=N)
//!     + ProducerValidationRejected
//!     N < max_validation_retries:
//!         → WaitingProducer(validation_attempt=N+1, feedback=[reason])
//!         + RunRole(Producer, feedback=[reason])
//!     N >= max_validation_retries:
//!         → Failed
//!
//! WaitingProducer + ProducerRejected { reason }
//!     → Failed
//!
//! WaitingProducer + ProducerFailed { reason }
//!     → Failed  (execution failure, not semantic rejection)
//!
//! WaitingCritic(pc) + CriticAccepted { content }
//!     → WaitingReferee(producer_content=pc, critic_advisory=AcceptedReview)
//!     + RunRole(Referee, …)
//!
//! WaitingCritic(pc) + CriticRejected { reason }
//!     → WaitingReferee(producer_content=pc, critic_advisory=RejectedReason)
//!     + RunRole(Referee, producer_content=pc, critic_content=reason)
//!     (Critic is advisory; Referee decides)
//!
//! WaitingCritic(…) + CriticFailed { reason }
//!     → Failed  (execution failure, not semantic rejection)
//!
//! WaitingReferee(pc, advisory) + RefereeAccepted
//!     → Complete { output: pc }   ← output is producer content
//!
//! WaitingReferee(…) + RefereeRejected { reason }
//!     feedback.len() < max_revisions:
//!         → WaitingProducer(feedback+[reason], validation_attempt=0)
//!         + RunRole(Producer, feedback+[reason])
//!     feedback.len() >= max_revisions:
//!         → Failed(reason=RevisionLimitExhausted)
//!
//! WaitingReferee(…) + RefereeFailed { reason }
//!     → Failed  (execution failure — must NOT enter the revision loop)
//!
//! Role mismatches → Failed (protocol violation)
//! ```

use crate::engine::{Machine, Transition};
use crate::machines::scheduler::FailureKind;

use super::effect::DeliberationEffect;
use super::event::DeliberationEvent;
use super::state::DeliberationState;
use super::types::DeliberationFailureReason;
use super::types::{
    CriticAdvisory, DeliberationOutput, DeliberationRole, DeliberationTerminalOutput,
    RevisionFeedback,
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
                state: DeliberationState::WaitingProducer {
                    request,
                    feedback: vec![],
                    validation_attempt: 0,
                },
            },

            // Producer accepted with artifact metadata from the handler → hand off to Validator.
            (
                DeliberationState::WaitingProducer {
                    request,
                    feedback,
                    validation_attempt,
                },
                DeliberationEvent::ProducerAccepted {
                    content,
                    artifact_changed,
                },
            ) => Transition {
                effects: vec![DeliberationEffect::ValidateProducer {
                    content: content.clone(),
                    artifact_changed,
                }],
                state: DeliberationState::WaitingValidator {
                    request,
                    producer_content: content,
                    feedback,
                    validation_attempt,
                },
            },

            // Validator accepted → hand off to Critic.
            (
                DeliberationState::WaitingValidator {
                    request,
                    producer_content,
                    feedback,
                    ..
                },
                DeliberationEvent::ProducerValidationAccepted { content },
            ) if producer_content == content => Transition {
                effects: vec![DeliberationEffect::RunRole {
                    role: DeliberationRole::Critic,
                    objective: request.objective.clone(),
                    context: request.context.clone(),
                    producer_content: Some(producer_content.clone()),
                    critic_content: None,
                    feedback: feedback.clone(),
                }],
                state: DeliberationState::WaitingCritic {
                    request,
                    producer_content,
                    feedback,
                },
            },

            // Validator rejected → retry Producer if validation retry budget remains.
            (
                DeliberationState::WaitingValidator {
                    request,
                    producer_content,
                    feedback,
                    validation_attempt,
                },
                DeliberationEvent::ProducerValidationRejected { content, retry },
            ) if producer_content == content => {
                if validation_attempt < retry.max_retries {
                    let validation_feedback = vec![RevisionFeedback {
                        reason: retry.feedback_reason,
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
                        state: DeliberationState::WaitingProducer {
                            request,
                            feedback,
                            validation_attempt: validation_attempt + 1,
                        },
                    }
                } else {
                    Self::failed_transition(
                        retry.failure_kind,
                        DeliberationFailureReason::ProducerValidationRetriesExhausted,
                        retry.failure_reason,
                    )
                }
            }

            // Producer rejected → failed.
            (
                DeliberationState::WaitingProducer { .. },
                DeliberationEvent::ProducerRejected { reason },
            ) => Self::failed_transition(
                FailureKind::UserTaskRejection,
                DeliberationFailureReason::ProducerRejected,
                reason,
            ),

            // Producer execution failure → terminal.
            (
                DeliberationState::WaitingProducer { .. },
                DeliberationEvent::ProducerFailed { kind, reason },
            ) => Self::failed_transition(
                kind,
                DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Producer,
                },
                reason,
            ),

            // Role mismatch while waiting for Producer → protocol violation.
            (
                DeliberationState::WaitingProducer { .. },
                DeliberationEvent::CriticAccepted { .. }
                | DeliberationEvent::CriticRejected { .. }
                | DeliberationEvent::CriticFailed { .. },
            ) => {
                let reason =
                    "protocol violation: expected Producer result but received Critic".to_string();
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            (
                DeliberationState::WaitingProducer { .. },
                DeliberationEvent::RefereeAccepted { .. }
                | DeliberationEvent::RefereeRejected { .. }
                | DeliberationEvent::RefereeFailed { .. },
            ) => {
                let reason =
                    "protocol violation: expected Producer result but received Referee".to_string();
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            // Critic accepted → hand off to Referee.
            (
                DeliberationState::WaitingCritic {
                    request,
                    producer_content,
                    feedback,
                },
                DeliberationEvent::CriticAccepted {
                    content: critic_content,
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
                    state: DeliberationState::WaitingReferee {
                        request,
                        producer_content,
                        critic_advisory,
                        feedback,
                    },
                }
            }

            // Critic rejected → route to Referee with a typed advisory reason.
            // The Critic is advisory; only the Referee is authoritative.
            (
                DeliberationState::WaitingCritic {
                    request,
                    producer_content,
                    feedback,
                },
                DeliberationEvent::CriticRejected { reason },
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
                    state: DeliberationState::WaitingReferee {
                        request,
                        producer_content,
                        critic_advisory,
                        feedback,
                    },
                }
            }

            // Critic execution failure → terminal.
            (
                DeliberationState::WaitingCritic { .. },
                DeliberationEvent::CriticFailed { kind, reason },
            ) => Self::failed_transition(
                kind,
                DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Critic,
                },
                reason,
            ),

            // Role mismatch while waiting for Critic → protocol violation.
            (
                DeliberationState::WaitingCritic { .. },
                DeliberationEvent::ProducerAccepted { .. }
                | DeliberationEvent::ProducerRejected { .. }
                | DeliberationEvent::ProducerFailed { .. },
            ) => {
                let reason =
                    "protocol violation: expected Critic result but received Producer".to_string();
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            (
                DeliberationState::WaitingCritic { .. },
                DeliberationEvent::RefereeAccepted { .. }
                | DeliberationEvent::RefereeRejected { .. }
                | DeliberationEvent::RefereeFailed { .. },
            ) => {
                let reason =
                    "protocol violation: expected Critic result but received Referee".to_string();
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            // Referee accepted → complete with producer content (not referee content).
            (
                DeliberationState::WaitingReferee {
                    producer_content, ..
                },
                DeliberationEvent::RefereeAccepted { .. },
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
                DeliberationState::WaitingReferee {
                    request, feedback, ..
                },
                DeliberationEvent::RefereeRejected { reason },
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
                        state: DeliberationState::WaitingProducer {
                            request,
                            feedback: new_feedback,
                            validation_attempt: 0,
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
                DeliberationState::WaitingReferee { .. },
                DeliberationEvent::RefereeFailed { kind, reason },
            ) => Self::failed_transition(
                kind,
                DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Referee,
                },
                reason,
            ),

            // Role mismatch while waiting for Referee → protocol violation.
            (
                DeliberationState::WaitingReferee { .. },
                DeliberationEvent::ProducerAccepted { .. }
                | DeliberationEvent::ProducerRejected { .. }
                | DeliberationEvent::ProducerFailed { .. },
            ) => {
                let reason =
                    "protocol violation: expected Referee result but received Producer".to_string();
                Self::failed_transition(
                    FailureKind::ProtocolFailure,
                    DeliberationFailureReason::ProtocolViolation,
                    reason,
                )
            }

            (
                DeliberationState::WaitingReferee { .. },
                DeliberationEvent::CriticAccepted { .. }
                | DeliberationEvent::CriticRejected { .. }
                | DeliberationEvent::CriticFailed { .. },
            ) => {
                let reason =
                    "protocol violation: expected Referee result but received Critic".to_string();
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
