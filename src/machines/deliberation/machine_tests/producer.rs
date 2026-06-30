use super::*;

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
                ..
            } if objective == "write a poem" && feedback.is_empty()
        ),
        "expected RunRole(Producer, feedback=[]), got {:?}",
        t.effects[0]
    );
}

#[test]
fn producer_acceptance_runs_validation_then_critic() {
    let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

    let validation = step(
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
            &validation.state,
            DeliberationState::Waiting {
                role: DeliberationRole::Producer,
                producer_content: Some(pc),
                critic_content: None,
                ..
            } if pc == "draft content"
        ),
        "expected Waiting(Producer validation, Some('draft content')), got {:?}",
        validation.state
    );

    assert_eq!(validation.effects.len(), 1);
    assert!(
        matches!(
            &validation.effects[0],
            DeliberationEffect::ValidateProducer {
                content,
                artifact_changed: false,
            } if content == "draft content"
        ),
        "expected ValidateProducer('draft content'), got {:?}",
        validation.effects[0]
    );

    let t = step(
        validation.state,
        DeliberationEvent::ProducerValidationReturned {
            content: "draft content".to_string(),
            result: ProducerValidationResult::Valid,
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
fn producer_validation_retry_runs_producer_with_validation_feedback() {
    let validating = step(
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
        validating,
        DeliberationEvent::ProducerValidationReturned {
            content: "draft content".to_string(),
            result: ProducerValidationResult::Retry {
                feedback_reason: "must be valid JSON".to_string(),
                max_retries: 2,
                failure_kind: FailureKind::PlannerValidationFailure,
                failure_reason: "planner validation failed".to_string(),
            },
        },
    );

    match &t.state {
        DeliberationState::Waiting {
            role: DeliberationRole::Producer,
            producer_validation,
            producer_content,
            ..
        } => {
            assert!(producer_content.is_none());
            assert_eq!(producer_validation.attempt, 1);
            assert_eq!(producer_validation.feedback[0].reason, "must be valid JSON");
        }
        other => panic!("expected Producer retry state, got {other:?}"),
    }
    assert!(matches!(
        &t.effects[0],
        DeliberationEffect::RunRole {
            role: DeliberationRole::Producer,
            feedback,
            ..
        } if feedback[0].reason == "must be valid JSON"
    ));
}

#[test]
fn producer_validation_retry_exhaustion_fails() {
    let validating = DeliberationState::Waiting {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            target_files: vec![],
            max_revisions: 0,
        },
        role: DeliberationRole::Producer,
        producer_content: Some("draft content".to_string()),
        critic_content: None,
        revision_count: 0,
        feedback: vec![],
        producer_validation: ProducerValidationState {
            attempt: 2,
            feedback: vec![RevisionFeedback {
                reason: "previous validation failure".to_string(),
            }],
        },
    };

    let t = step(
        validating,
        DeliberationEvent::ProducerValidationReturned {
            content: "draft content".to_string(),
            result: ProducerValidationResult::Retry {
                feedback_reason: "still invalid".to_string(),
                max_retries: 2,
                failure_kind: FailureKind::PlannerValidationFailure,
                failure_reason: "planner validation failed".to_string(),
            },
        },
    );

    assert!(matches!(
        &t.state,
        DeliberationState::Failed {
            kind: FailureKind::PlannerValidationFailure,
            reason,
        } if reason == "planner validation failed"
    ));
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
        matches!(&t.state, DeliberationState::Failed { reason, .. } if reason == "out of ideas"),
        "expected Failed, got {:?}",
        t.state
    );

    assert_eq!(t.effects.len(), 1);
    assert!(
        matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason, .. } if reason == "out of ideas"),
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
        DeliberationEffect::ReturnFailed { reason, .. } => reason,
        other => panic!("expected ReturnFailed, got {:?}", other),
    };
    assert!(
        reason.contains("protocol violation"),
        "expected 'protocol violation' in reason, got: {reason}"
    );
}

#[test]
fn producer_failed_is_terminal() {
    let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

    let t = step(
        waiting,
        DeliberationEvent::RoleReturned {
            role: DeliberationRole::Producer,
            result: RoleResult::Failed {
                kind: FailureKind::ProviderFailure,
                reason: "timeout".to_string(),
            },
        },
    );

    assert!(
        matches!(&t.state, DeliberationState::Failed { reason, .. } if reason == "timeout"),
        "expected Failed, got {:?}",
        t.state
    );

    assert_eq!(t.effects.len(), 1);
    assert!(
        matches!(&t.effects[0], DeliberationEffect::ReturnFailed { reason, .. } if reason == "timeout"),
        "expected ReturnFailed, got {:?}",
        t.effects[0]
    );
}
