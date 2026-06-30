use super::*;

#[test]
fn output_returns_failed_for_failed_state() {
    let failed_state = DeliberationState::Failed {
        kind: FailureKind::DeliberationFailure,
        reason: DeliberationFailureReason::InvalidState,
        message: "something went wrong".to_string(),
    };
    let output = machine().output(&failed_state);
    match output {
        Some(DeliberationTerminalOutput::Failed {
            reason, message, ..
        }) => {
            assert_eq!(reason, DeliberationFailureReason::InvalidState);
            assert_eq!(message, "something went wrong");
        }
        other => panic!("expected Some(Failed), got {:?}", other),
    }
}
