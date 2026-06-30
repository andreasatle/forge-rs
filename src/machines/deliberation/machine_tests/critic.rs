use super::*;

#[test]
fn critic_acceptance_runs_referee() {
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

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
                critic_advisory: Some(CriticAdvisory::AcceptedReview { content }),
                ..
            } if pc == "draft content" && content == "looks good"
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
fn critic_rejection_routes_to_referee() {
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

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
        matches!(
            &t.state,
            DeliberationState::Waiting {
                role: DeliberationRole::Referee,
                producer_content: Some(pc),
                critic_advisory: Some(CriticAdvisory::RejectedReason { reason }),
                ..
            } if pc == "draft content" && reason == "too short"
        ),
        "expected Waiting(Referee) with rejected critic advisory, got {:?}",
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
            } if pc == "draft content" && cc == "too short"
        ),
        "expected RunRole(Referee) with critic rejection reason, got {:?}",
        t.effects[0]
    );
}

#[test]
fn critic_rejection_stores_typed_rejected_advisory() {
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

    let t = step(
        after_producer,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Critic,
            result: RoleResult::Rejected {
                reason: "the haiku is not following the 5-7-5 syllable structure".to_string(),
            },
        },
    );

    let critic_reason = match &t.state {
        DeliberationState::Waiting {
            critic_advisory: Some(CriticAdvisory::RejectedReason { reason }),
            ..
        } => reason,
        other => panic!(
            "expected Waiting with rejected critic advisory, got {:?}",
            other
        ),
    };
    assert!(
        critic_reason.contains("5-7-5"),
        "critic advisory must contain the original critique; got: {critic_reason}"
    );
}

#[test]
fn critic_rejection_runs_referee_instead_of_failing() {
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

    let t = step(
        after_producer,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Critic,
            result: RoleResult::Rejected {
                reason: "not good enough".to_string(),
            },
        },
    );

    assert!(
        matches!(
            &t.effects[..],
            [DeliberationEffect::RunRole {
                role: DeliberationRole::Referee,
                ..
            }]
        ),
        "Critic rejection must run Referee, got {:?}",
        t.effects
    );
}

#[test]
fn critic_missing_producer_content_fails() {
    let invalid_state = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 0,
        },
        role: DeliberationRole::Critic,
        producer_content: None,
        critic_advisory: None,
        feedback: vec![],
        producer_validation: producer_validation(),
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

    assert!(
        t.effects.is_empty(),
        "terminal failure must not emit effects"
    );
    let reason = match &t.state {
        DeliberationState::Failed { reason, .. } => reason,
        other => panic!("expected Failed, got {:?}", other),
    };
    assert_eq!(reason, &DeliberationFailureReason::InvalidState);
}

#[test]
fn role_mismatch_while_waiting_critic_fails() {
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft",
    );

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

#[test]
fn critic_failed_is_terminal() {
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

    let t = step(
        after_producer,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Critic,
            result: RoleResult::Failed {
                kind: FailureKind::ProviderFailure,
                reason: "provider unavailable".to_string(),
            },
        },
    );

    assert!(
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Critic
                },
                message,
                ..
            } if message == "provider unavailable"
        ),
        "expected Failed, got {:?}",
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
                role: DeliberationRole::Critic
            },
            message,
            ..
        }) if message == "provider unavailable"
    ));
}
