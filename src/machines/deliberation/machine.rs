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
//! Waiting(Producer) + RoleReturned(Producer, Failed { reason })
//!     → Failed + ReturnFailed  (execution failure, not semantic rejection)
//!
//! Waiting(Critic, Some(pc)) + RoleReturned(Critic, Accepted { content })
//!     → Waiting(Referee, producer_content=Some(pc), critic_content=Some(content))
//!     + RunRole(Referee, …)
//!
//! Waiting(Critic, Some(pc)) + RoleReturned(Critic, Rejected { reason })
//!     → Waiting(Referee, producer_content=Some(pc), critic_content=Some("Critic rejected: {reason}"))
//!     + RunRole(Referee, producer_content=Some(pc), critic_content=Some("Critic rejected: {reason}"))
//!     (Critic is advisory; Referee decides)
//!
//! Waiting(Critic, Some(_)) + RoleReturned(Critic, Failed { reason })
//!     → Failed + ReturnFailed  (execution failure, not semantic rejection)
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
//! Waiting(Referee, Some(_), Some(_)) + RoleReturned(Referee, Failed { reason })
//!     → Failed + ReturnFailed  (execution failure — must NOT enter the revision loop)
//!
//! Waiting(Referee, None, _) or Waiting(Referee, _, None) + RoleReturned(…)
//!     → Failed + ReturnFailed  (invalid deliberation state)
//!
//! Role mismatches → Failed + ReturnFailed (protocol violation)
//! ```

use crate::engine::{Machine, Transition};
use crate::machines::scheduler::FailureKind;

use super::effect::DeliberationEffect;
use super::event::{DeliberationEvent, RoleResult};
use super::state::{
    DeliberationOutput, DeliberationRole, DeliberationState, DeliberationTerminalOutput,
    RevisionFeedback,
};

/// The deliberation machine. All durable data travels in `DeliberationState`.
pub struct DeliberationMachine;

impl DeliberationMachine {
    fn failed_transition(
        kind: FailureKind,
        reason: String,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        Transition {
            effects: vec![DeliberationEffect::ReturnFailed {
                kind,
                reason: reason.clone(),
            }],
            state: DeliberationState::Failed { kind, reason },
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
                    target_files: request.target_files.clone(),
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
                    target_files: request.target_files.clone(),
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
            ) => Self::failed_transition(FailureKind::UserTaskRejection, reason),

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
            ) => Self::failed_transition(kind, reason),

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
                Self::failed_transition(FailureKind::ProtocolFailure, reason)
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
                Self::failed_transition(FailureKind::DeliberationFailure, reason)
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
                    target_files: request.target_files.clone(),
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

            // Critic rejected → route to Referee with the rejection reason as critic_content.
            // The Critic is advisory; only the Referee is authoritative.
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
                    result: RoleResult::Rejected { reason },
                },
            ) => {
                let critic_content = format!("Critic rejected: {reason}");
                Transition {
                    effects: vec![DeliberationEffect::RunRole {
                        role: DeliberationRole::Referee,
                        objective: request.objective.clone(),
                        target_files: request.target_files.clone(),
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
            ) => Self::failed_transition(kind, reason),

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
                Self::failed_transition(FailureKind::ProtocolFailure, reason)
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
                Self::failed_transition(FailureKind::DeliberationFailure, reason)
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
                            target_files: request.target_files.clone(),
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
                    Self::failed_transition(FailureKind::DeliberationFailure, fail_reason)
                }
            }

            // Referee execution failure → terminal. Must NOT enter the revision loop.
            (
                DeliberationState::Waiting {
                    role: DeliberationRole::Referee,
                    producer_content: Some(_),
                    critic_content: Some(_),
                    ..
                },
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Referee,
                    result: RoleResult::Failed { kind, reason },
                },
            ) => Self::failed_transition(kind, reason),

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
                Self::failed_transition(FailureKind::ProtocolFailure, reason)
            }

            (state, event) => {
                let reason = format!("invalid transition: state={state:?}, event={event:?}");
                Self::failed_transition(FailureKind::ProtocolFailure, reason)
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
            DeliberationState::Failed { kind, reason } => {
                Some(DeliberationTerminalOutput::Failed {
                    kind: *kind,
                    reason: reason.clone(),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "machine_tests/mod.rs"]
mod machine_tests;
