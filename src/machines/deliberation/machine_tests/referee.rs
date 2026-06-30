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
        matches!(&t.state, DeliberationState::Failed { reason, .. } if reason.contains("revision limit exhausted")),
        "expected Failed with 'revision limit exhausted', got {:?}",
        t.state
    );

    assert_eq!(t.effects.len(), 1);
    assert!(
        matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason, .. } if reason.contains("revision limit exhausted")),
        "expected ReturnFailed with 'revision limit exhausted', got {:?}",
        t.effects[0]
    );
}

#[test]
fn referee_missing_critic_content_fails() {
    let invalid_state = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            target_files: vec![],
            max_revisions: 0,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_content: None,
        feedback: vec![],
        producer_validation: producer_validation(),
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
        DeliberationEffect::ReturnFailed { reason, .. } => reason,
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
            target_files: vec![],
            max_revisions: 0,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_content: Some("looks good".to_string()),
        feedback: vec![],
        producer_validation: producer_validation(),
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
            DeliberationEffect::ReturnFailed { reason, .. } => reason,
            other => panic!("expected ReturnFailed, got {:?}", other),
        };
        assert!(
            reason.contains("protocol violation"),
            "expected 'protocol violation' in reason, got: {reason}"
        );
    }
}

#[test]
fn referee_failed_is_terminal() {
    // max_revisions=1 to confirm Failed does not enter the revision loop
    // even when revisions are available.
    let waiting_referee = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            target_files: vec![],
            max_revisions: 1,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_content: Some("review".to_string()),
        feedback: vec![],
        producer_validation: producer_validation(),
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
        matches!(&t.state, DeliberationState::Failed { reason, .. } if reason == "authentication error"),
        "expected Failed (not a revision loop), got {:?}",
        t.state
    );

    assert_eq!(t.effects.len(), 1);
    assert!(
        matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason, .. } if reason == "authentication error"),
        "expected ReturnFailed, got {:?}",
        t.effects[0]
    );
}

#[test]
fn referee_rejected_still_revises() {
    // Rejected (semantic outcome) continues to loop; Failed (execution) must not.
    let waiting_referee = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            target_files: vec![],
            max_revisions: 1,
        },
        role: DeliberationRole::Referee,
        producer_content: Some("draft".to_string()),
        critic_content: Some("review".to_string()),
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
}
