use super::*;

#[test]
fn referee_rejection_after_critic_rejection_loops_when_revisions_remain() {
    let after_producer = producer_accepts(
        step(
            DeliberationState::Ready {
                request: crate::machines::deliberation::state::DeliberationRequest {
                    objective: "write a poem".to_string(),
                    context: crate::machines::deliberation::DeliberationContext::default(),
                    max_revisions: 1,
                },
            },
            DeliberationEvent::Start,
        )
        .state,
        "draft content",
    );

    // Critic rejects → routes to Referee.
    let after_critic_rejection = step(
        after_producer,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Critic,
            result: RoleResult::Rejected {
                reason: "too short".to_string(),
            },
        },
    )
    .state;

    // Referee also rejects — should loop back to Producer.
    let t = step(
        after_critic_rejection,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Referee,
            result: RoleResult::Rejected {
                reason: "still not acceptable".to_string(),
            },
        },
    );

    assert!(
        matches!(
            &t.state,
            DeliberationState::Waiting {
                role: DeliberationRole::Producer,
                feedback,
                ..
            } if feedback.len() == 1
        ),
        "expected Waiting(Producer) revision loop, got {:?}",
        t.state
    );
    assert_eq!(t.effects.len(), 1);
    assert!(
        matches!(
            &t.effects[0],
            DeliberationEffect::RunRole {
                role: DeliberationRole::Producer,
                ..
            }
        ),
        "expected RunRole(Producer), got {:?}",
        t.effects[0]
    );
}

#[test]
fn referee_rejection_after_critic_rejection_exhausts_budget_when_no_revisions_remain() {
    // max_revisions=0: any Referee rejection must fail immediately.
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

    // Critic rejects → routes to Referee.
    let after_critic_rejection = step(
        after_producer,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Critic,
            result: RoleResult::Rejected {
                reason: "too short".to_string(),
            },
        },
    )
    .state;

    // Referee rejects — no revisions remain → fail.
    let t = step(
        after_critic_rejection,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Referee,
            result: RoleResult::Rejected {
                reason: "still not acceptable".to_string(),
            },
        },
    );

    assert!(
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::RevisionLimitExhausted,
                ..
            }
        ),
        "expected Failed with 'revision limit exhausted', got {:?}",
        t.state
    );
    assert!(
        t.effects.is_empty(),
        "terminal failure must not emit effects"
    );
    assert!(matches!(
        machine().output(&t.state),
        Some(DeliberationTerminalOutput::Failed {
            reason: DeliberationFailureReason::RevisionLimitExhausted,
            ..
        })
    ));
}

#[test]
fn referee_rejection_loops_to_producer_with_feedback() {
    let waiting_referee = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 1,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_advisory: Some(CriticAdvisory::AcceptedReview {
            content: "review".to_string(),
        }),
        feedback: vec![],
        producer_validation: producer_validation(),
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

    // State must be Waiting(Producer) with one feedback entry.
    match &t.state {
        DeliberationState::Waiting {
            role: DeliberationRole::Producer,
            feedback,
            producer_content,
            critic_advisory,
            ..
        } => {
            assert_eq!(feedback.len(), 1, "feedback should have one entry");
            assert_eq!(
                feedback[0].reason, "needs changes",
                "feedback reason mismatch"
            );
            assert!(
                producer_content.is_none(),
                "producer_content should be None"
            );
            assert!(critic_advisory.is_none(), "critic_advisory should be None");
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
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 1,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_advisory: Some(CriticAdvisory::AcceptedReview {
            content: "review".to_string(),
        }),
        feedback: vec![RevisionFeedback {
            reason: "earlier rejection".to_string(),
        }],
        producer_validation: producer_validation(),
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
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::RevisionLimitExhausted,
                ..
            }
        ),
        "expected Failed with 'revision limit exhausted', got {:?}",
        t.state
    );

    assert!(
        t.effects.is_empty(),
        "terminal failure must not emit effects"
    );
    assert!(matches!(
        machine().output(&t.state),
        Some(DeliberationTerminalOutput::Failed {
            reason: DeliberationFailureReason::RevisionLimitExhausted,
            ..
        })
    ));
}

#[test]
fn max_revisions_zero_fails_on_first_referee_rejection() {
    let waiting_referee = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 0,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_advisory: Some(CriticAdvisory::AcceptedReview {
            content: "review".to_string(),
        }),
        feedback: vec![],
        producer_validation: producer_validation(),
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
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::RevisionLimitExhausted,
                ..
            }
        ),
        "expected Failed with 'revision limit exhausted', got {:?}",
        t.state
    );

    assert!(
        t.effects.is_empty(),
        "terminal failure must not emit effects"
    );
    assert!(matches!(
        machine().output(&t.state),
        Some(DeliberationTerminalOutput::Failed {
            reason: DeliberationFailureReason::RevisionLimitExhausted,
            ..
        })
    ));
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
        type Output = DeliberationTerminalOutput;

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
                    DeliberationEvent::ProducerAccepted {
                        content,
                        artifact_changed: false,
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
                DeliberationEffect::ValidateProducer { content, .. } => {
                    DeliberationEvent::ProducerValidationReturned {
                        content,
                        result: ProducerValidationResult::Valid,
                    }
                }
            }
        }

        fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
            DeliberationMachine.output(state)
        }
    }

    let initial = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 1,
        },
    };

    let fake = FakeMachine {
        producer_call: std::cell::Cell::new(0),
    };
    let output = run_machine(fake, initial);
    match output {
        DeliberationTerminalOutput::Complete(out) => assert_eq!(
            out.content, "draft v2",
            "final output should be revised producer content"
        ),
        other => panic!("expected Complete, got {:?}", other),
    }
}

// Smoke test: Producer → Accepted("draft"), Critic → Accepted("looks good"),
// Referee → Accepted("approved"). Final output must be "draft" (producer content).
