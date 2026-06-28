use super::*;

#[test]
fn output_returns_failed_for_failed_state() {
    let failed_state = DeliberationState::Failed {
        kind: FailureKind::DeliberationFailure,
        reason: "something went wrong".to_string(),
    };
    let output = machine().output(&failed_state);
    match output {
        Some(DeliberationTerminalOutput::Failed { reason, .. }) => {
            assert_eq!(reason, "something went wrong");
        }
        other => panic!("expected Some(Failed), got {:?}", other),
    }
}
