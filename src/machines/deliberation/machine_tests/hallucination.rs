use super::*;

#[test]
fn critic_rejection_with_structural_defect_claim_is_discarded_and_retried() {
    // Invariant: a Critic rejection asserting a syntax/JSON-malformation
    // defect against content that already passed grammar-constrained
    // decoding and structural validation is provably false. It must not be
    // forwarded to the Referee as advisory content, must not appear as
    // CriticAdvisory in state, and must re-request a fresh Critic decision.
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

    let t = step(
        after_producer,
        DeliberationEvent::CriticRejected {
            reason: "This is invalid JSON with a syntax error near the tasks array.".to_string(),
        },
    );

    assert!(
        matches!(
            &t.state,
            DeliberationState::WaitingCritic {
                producer_content,
                feedback,
                ..
            } if producer_content == "draft content" && feedback.is_empty()
        ),
        "expected unchanged WaitingCritic, got {:?}",
        t.state
    );

    assert_eq!(t.effects.len(), 1);
    match &t.effects[0] {
        DeliberationEffect::DiscardHallucinatedRejection {
            role: DeliberationRole::Critic,
            reason,
            retry,
        } => {
            assert!(reason.contains("syntax error"));
            assert!(
                matches!(
                    retry.as_ref(),
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Critic,
                        producer_content: Some(pc),
                        critic_content: None,
                        ..
                    } if pc == "draft content"
                ),
                "expected retry RunRole(Critic), got {:?}",
                retry
            );
        }
        other => panic!(
            "expected DiscardHallucinatedRejection(Critic), got {:?}",
            other
        ),
    }
}

#[test]
fn referee_rejection_with_structural_defect_claim_is_discarded_and_retried() {
    // Invariant: a Referee rejection asserting a structural JSON defect
    // against already-validated content must be discarded, must not consume
    // any revision budget, and must re-request a fresh Referee decision.
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
            content: "looks good".to_string(),
        },
        feedback: vec![],
    };

    let t = step(
        waiting_referee,
        DeliberationEvent::RefereeRejected {
            reason: "The output has an unclosed field and is malformed JSON.".to_string(),
        },
    );

    assert!(
        matches!(
            &t.state,
            DeliberationState::WaitingReferee {
                producer_content,
                critic_advisory: CriticAdvisory::AcceptedReview { content },
                feedback,
                ..
            } if producer_content == "draft" && content == "looks good" && feedback.is_empty()
        ),
        "expected unchanged WaitingReferee (no revision budget consumed), got {:?}",
        t.state
    );

    assert_eq!(t.effects.len(), 1);
    match &t.effects[0] {
        DeliberationEffect::DiscardHallucinatedRejection {
            role: DeliberationRole::Referee,
            reason,
            retry,
        } => {
            assert!(reason.contains("malformed JSON"));
            assert!(
                matches!(
                    retry.as_ref(),
                    DeliberationEffect::RunRole {
                        role: DeliberationRole::Referee,
                        producer_content: Some(pc),
                        critic_content: Some(cc),
                        ..
                    } if pc == "draft" && cc == "looks good"
                ),
                "expected retry RunRole(Referee), got {:?}",
                retry
            );
        }
        other => panic!(
            "expected DiscardHallucinatedRejection(Referee), got {:?}",
            other
        ),
    }
}

#[test]
fn referee_rejection_with_structural_defect_claim_does_not_exhaust_zero_revision_budget() {
    // Invariant: even with max_revisions=0 (no legitimate revisions allowed
    // at all), a hallucinated structural-defect claim is still discarded
    // rather than treated as a real rejection that exhausts the budget.
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

    let t = step(
        waiting_referee,
        DeliberationEvent::RefereeRejected {
            reason: "Rejected: unexpected token, this is not valid JSON.".to_string(),
        },
    );

    assert!(
        matches!(&t.state, DeliberationState::WaitingReferee { .. }),
        "expected discard to bypass revision-budget exhaustion, got {:?}",
        t.state
    );
    assert_eq!(t.effects.len(), 1);
    assert!(matches!(
        &t.effects[0],
        DeliberationEffect::DiscardHallucinatedRejection {
            role: DeliberationRole::Referee,
            ..
        }
    ));
}

#[test]
fn genuine_semantic_rejections_are_unaffected_by_structural_defect_check() {
    // Invariant: the discard check must not misclassify legitimate judgment
    // rejections (clarity, overlap, incompleteness) as hallucinated claims —
    // both Critic and Referee rejection paths must proceed as before.
    let after_producer = producer_accepts(
        step(ready("write a poem"), DeliberationEvent::Start).state,
        "draft content",
    );

    let t = step(
        after_producer,
        DeliberationEvent::CriticRejected {
            reason: "The task lacks a clear definition of expected behavior.".to_string(),
        },
    );

    assert!(
        matches!(&t.state, DeliberationState::WaitingReferee { .. }),
        "expected normal CriticRejected routing to Referee, got {:?}",
        t.state
    );
    assert_eq!(t.effects.len(), 1);
    assert!(matches!(
        &t.effects[0],
        DeliberationEffect::RunRole {
            role: DeliberationRole::Referee,
            ..
        }
    ));
}
