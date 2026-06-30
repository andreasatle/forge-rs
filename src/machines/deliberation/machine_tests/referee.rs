use super::*;

#[test]
fn referee_acceptance_completes_with_producer_content() {
    let after_critic = step(
        producer_accepts(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            "draft content",
        ),
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

    assert!(
        t.effects.is_empty(),
        "terminal completion must not emit effects"
    );
    assert!(
        matches!(
            machine().output(&t.state),
            Some(DeliberationTerminalOutput::Complete(output)) if output.content == "draft content"
        ),
        "expected Complete output with producer content"
    );
}

#[test]
fn referee_rejection_fails_when_no_revisions_allowed() {
    let after_critic = step(
        producer_accepts(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            "draft content",
        ),
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
fn role_mismatch_while_waiting_referee_fails() {
    let waiting_referee = DeliberationState::WaitingReferee {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 0,
        },
        producer_content: "draft".to_string(),
        critic_advisory: CriticAdvisory::AcceptedReview {
            content: "looks good".to_string(),
        },
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

        assert!(
            t.effects.is_empty(),
            "terminal failure must not emit effects"
        );
        let reason = match &t.state {
            DeliberationState::Failed { reason, .. } => reason,
            other => panic!("expected Failed, got {:?}", other),
        };
        assert_eq!(reason, &DeliberationFailureReason::ProtocolViolation);
    }
}

#[test]
fn referee_failed_is_terminal() {
    // max_revisions=1 to confirm Failed does not enter the revision loop
    // even when revisions are available.
    let waiting_referee = DeliberationState::WaitingReferee {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 1,
        },
        producer_content: "draft".to_string(),
        critic_advisory: CriticAdvisory::AcceptedReview {
            content: "review".to_string(),
        },
        feedback: vec![],
    };

    let t = step(
        waiting_referee,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Referee,
            result: RoleResult::Failed {
                kind: FailureKind::ProviderTerminalFailure,
                reason: "authentication error".to_string(),
            },
        },
    );

    assert!(
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Referee
                },
                message,
                ..
            } if message == "authentication error"
        ),
        "expected Failed (not a revision loop), got {:?}",
        t.state
    );
    assert!(
        t.effects.is_empty(),
        "terminal failure must not emit effects"
    );
    assert!(matches!(
        machine().output(&t.state),
        Some(DeliberationTerminalOutput::Failed {
            reason: DeliberationFailureReason::RoleFailed {
                role: DeliberationRole::Referee
            },
            message,
            ..
        }) if message == "authentication error"
    ));
}

#[test]
fn referee_rejected_still_revises() {
    // Rejected (semantic outcome) continues to loop; Failed (execution) must not.
    let waiting_referee = DeliberationState::WaitingReferee {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 1,
        },
        producer_content: "draft".to_string(),
        critic_advisory: CriticAdvisory::AcceptedReview {
            content: "review".to_string(),
        },
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

    assert!(
        matches!(
            &t.state,
            DeliberationState::WaitingProducer {
                feedback,
                ..
            } if feedback.len() == 1
        ),
        "expected WaitingProducer revision loop, got {:?}",
        t.state
    );
}
