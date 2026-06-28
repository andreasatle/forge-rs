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
fn producer_acceptance_runs_critic() {
    let waiting = step(ready("write a poem"), DeliberationEvent::Start).state;

    let t = step(
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
