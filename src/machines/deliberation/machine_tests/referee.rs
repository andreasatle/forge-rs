use super::*;

#[test]
fn referee_acceptance_completes_with_producer_content() {
    let after_critic = step(
        producer_accepts(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            "draft content",
        ),
        DeliberationEvent::CriticAccepted {
            content: "looks good".to_string(),
        },
    )
    .state;

    let t = step(
        after_critic,
        DeliberationEvent::RefereeAccepted {
            content: "referee notes (ignored)".to_string(),
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
    // Both a fully-driven state (via producer_accepts + CriticAccepted) and a
    // directly-constructed WaitingReferee state must hit the same
    // RevisionLimitExhausted terminal transition — the construction style is
    // not a meaningful axis to test separately.
    let via_full_flow = step(
        producer_accepts(
            step(ready("write a poem"), DeliberationEvent::Start).state,
            "draft content",
        ),
        DeliberationEvent::CriticAccepted {
            content: "looks good".to_string(),
        },
    )
    .state;

    let via_direct_construction = DeliberationState::WaitingReferee {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            node_kind: crate::machines::scheduler::NodeKind::Work,
            test_plan_context: crate::machines::scheduler::TestPlanContext::default(),
            max_revisions: 0,
            worker_role: None,
        },
        producer_content: "draft".to_string(),
        critic_advisory: CriticAdvisory::AcceptedReview {
            content: "looks good".to_string(),
        },
        feedback: vec![],
    };

    for waiting_referee in [via_full_flow, via_direct_construction] {
        let t = step(
            waiting_referee,
            DeliberationEvent::RefereeRejected {
                reason: "not acceptable".to_string(),
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
}

#[test]
fn role_mismatch_while_waiting_referee_fails() {
    let waiting_referee = DeliberationState::WaitingReferee {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            node_kind: crate::machines::scheduler::NodeKind::Work,
            test_plan_context: crate::machines::scheduler::TestPlanContext::default(),
            max_revisions: 0,
            worker_role: None,
        },
        producer_content: "draft".to_string(),
        critic_advisory: CriticAdvisory::AcceptedReview {
            content: "looks good".to_string(),
        },
        feedback: vec![],
    };

    for wrong_event in [
        DeliberationEvent::ProducerAccepted {
            content: "unexpected".to_string(),
            artifact_changed: false,
        },
        DeliberationEvent::CriticAccepted {
            content: "unexpected".to_string(),
        },
    ] {
        let t = step(waiting_referee.clone(), wrong_event);

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
            node_kind: crate::machines::scheduler::NodeKind::Work,
            test_plan_context: crate::machines::scheduler::TestPlanContext::default(),
            max_revisions: 1,
            worker_role: None,
        },
        producer_content: "draft".to_string(),
        critic_advisory: CriticAdvisory::AcceptedReview {
            content: "review".to_string(),
        },
        feedback: vec![],
    };

    let t = step(
        waiting_referee,
        DeliberationEvent::RefereeFailed {
            kind: FailureKind::ProviderTerminalFailure,
            reason: "authentication error".to_string(),
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
