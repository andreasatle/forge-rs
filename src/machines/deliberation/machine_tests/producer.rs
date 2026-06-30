use super::*;

#[test]
fn ready_start_runs_producer() {
    let t = step(ready("write a poem"), DeliberationEvent::Start);

    assert!(
        matches!(&t.state, DeliberationState::WaitingProducer { .. }),
        "expected WaitingProducer, got {:?}",
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
        DeliberationEvent::ProducerAccepted {
            content: "draft content".to_string(),
            artifact_changed: false,
        },
    );

    assert!(
        matches!(
            &validation.state,
            DeliberationState::WaitingValidator {
                producer_content,
                ..
            } if producer_content == "draft content"
        ),
        "expected WaitingValidator('draft content'), got {:?}",
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
            DeliberationState::WaitingCritic {
                producer_content,
                ..
            } if producer_content == "draft content"
        ),
        "expected WaitingCritic('draft content'), got {:?}",
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
        DeliberationEvent::ProducerAccepted {
            content: "draft content".to_string(),
            artifact_changed: false,
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
        DeliberationState::WaitingProducer {
            validation_attempt, ..
        } => {
            assert_eq!(
                *validation_attempt, 1,
                "validation_attempt must increment to 1 after first retry"
            );
        }
        other => panic!("expected WaitingProducer retry state, got {other:?}"),
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
    let validating = DeliberationState::WaitingValidator {
        request: DeliberationRequest {
            objective: "write a poem".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            max_revisions: 0,
        },
        producer_content: "draft content".to_string(),
        feedback: vec![],
        validation_attempt: 2,
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
            message,
        } if *reason == DeliberationFailureReason::ProducerValidationRetriesExhausted
            && message == "planner validation failed"
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
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::ProducerRejected,
                message,
                ..
            } if message == "out of ideas"
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
            reason: DeliberationFailureReason::ProducerRejected,
            message,
            ..
        }) if message == "out of ideas"
    ));
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
        matches!(
            &t.state,
            DeliberationState::Failed {
                reason: DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Producer
                },
                message,
                ..
            } if message == "timeout"
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
                role: DeliberationRole::Producer
            },
            message,
            ..
        }) if message == "timeout"
    ));
}
