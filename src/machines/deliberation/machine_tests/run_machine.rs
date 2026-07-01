use super::*;

#[test]
fn run_machine_deliberation_smoke_test() {
    struct FakeMachine;

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
                } => DeliberationEvent::ProducerAccepted {
                    content: "draft".to_string(),
                    artifact_changed: false,
                },
                DeliberationEffect::RunRole {
                    role: DeliberationRole::Critic,
                    ..
                } => DeliberationEvent::CriticAccepted {
                    content: "looks good".to_string(),
                },
                DeliberationEffect::RunRole {
                    role: DeliberationRole::Referee,
                    ..
                } => DeliberationEvent::RefereeAccepted {
                    content: "approved".to_string(),
                },
                DeliberationEffect::ValidateProducer { content, .. } => {
                    DeliberationEvent::ProducerValidationAccepted { content }
                }
            }
        }

        fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
            DeliberationMachine.output(state)
        }
    }

    let initial = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: "smoke test".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            node_kind: crate::machines::scheduler::NodeKind::Work,
            test_plan_context: crate::machines::scheduler::TestPlanContext::default(),
            max_revisions: 0,
        },
    };

    let output = run_machine(FakeMachine, initial);
    match output {
        DeliberationTerminalOutput::Complete(out) => assert_eq!(out.content, "draft"),
        other => panic!("expected Complete, got {:?}", other),
    }
}

#[test]
fn run_machine_provider_failure_smoke_test() {
    struct FakeMachine;

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
                } => DeliberationEvent::ProducerFailed {
                    kind: FailureKind::ProviderFailure,
                    reason: "timeout".into(),
                },
                other => panic!("unexpected effect: {:?}", other),
            }
        }

        fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
            DeliberationMachine.output(state)
        }
    }

    let initial = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: "write something".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            node_kind: crate::machines::scheduler::NodeKind::Work,
            test_plan_context: crate::machines::scheduler::TestPlanContext::default(),
            max_revisions: 0,
        },
    };

    let output = run_machine(FakeMachine, initial);
    match &output {
        DeliberationTerminalOutput::Failed {
            reason, message, ..
        } => {
            assert_eq!(
                reason,
                &DeliberationFailureReason::RoleFailed {
                    role: DeliberationRole::Producer
                }
            );
            assert_eq!(message, "timeout");
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}

#[test]
fn run_machine_producer_rejection_returns_failed_output() {
    struct FakeMachine;

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
                } => DeliberationEvent::ProducerRejected {
                    reason: "bad draft".into(),
                },
                other => panic!("unexpected effect: {:?}", other),
            }
        }

        fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
            DeliberationMachine.output(state)
        }
    }

    let initial = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: "write something".to_string(),
            context: crate::machines::deliberation::DeliberationContext::default(),
            node_kind: crate::machines::scheduler::NodeKind::Work,
            test_plan_context: crate::machines::scheduler::TestPlanContext::default(),
            max_revisions: 0,
        },
    };

    let output = run_machine(FakeMachine, initial);
    match &output {
        DeliberationTerminalOutput::Failed {
            reason, message, ..
        } => {
            assert_eq!(reason, &DeliberationFailureReason::ProducerRejected);
            assert_eq!(message, "bad draft");
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}
