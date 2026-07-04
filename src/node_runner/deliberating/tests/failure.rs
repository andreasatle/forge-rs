use super::*;

#[test]
fn deliberating_runner_preserves_deliberation_failure() {
    let provider = ScriptedProvider::from_strs(&[
        "not valid json at all",
        "still not valid json",
        "also not valid json",
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("do something"), &NoopTelemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(matches!(failure.recovery, RecoveryAction::Retry { .. }));
}

#[test]
fn retryable_failure_produces_retry_action() {
    let provider = ScriptedProvider::failing(
        ProviderErrorKind::Retryable,
        "connection refused on http://localhost:8080/completion",
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(work_request("do something"), &telemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        failure
            .message
            .contains("provider error (Retryable): connection refused"),
        "failure message must include the original provider error; got: {}",
        failure.message
    );
    let RecoveryAction::Retry { message } = &failure.recovery else {
        panic!(
            "retryable provider error must produce Retry recovery; got {:?}",
            failure.recovery
        );
    };
    assert!(
        message.contains("provider error (Retryable): connection refused"),
        "retry message must include the original reason; got: {message}"
    );
    let records = telemetry.into_records();
    let classified = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FailureClassified { .. }
        )
    });
    assert!(
        classified.is_some(),
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "Retry");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn semantic_failure_produces_split_action() {
    // Revision limit exhaustion is a semantic failure regardless of the Referee's
    // rejection wording. The runner allows 1 revision, so both Referee rejections
    // are needed to exhaust the budget and produce Split (re-plan before
    // escalating to a stronger model).
    let cases: &[(&str, &str, &str)] = &[
        (
            "semantic-split",
            "needs improvement",
            "still not good enough",
        ),
        ("deliberation-split", "task too large", "task too large"),
    ];

    for (dir, first_reason, second_reason) in cases {
        let temp = TempDir::new(dir);
        let rejected_1 = format!(r#"{{"status":"rejected","reason":"{first_reason}"}}"#);
        let rejected_2 = format!(r#"{{"status":"rejected","reason":"{second_reason}"}}"#);
        let provider = ScriptedProvider::from_strs(&[
            // Round 1: Producer → Critic → Referee rejects → revision loop.
            r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
            r#"{"summary":"draft v1"}"#,
            r#"{"tool":"read_file","path":"output.txt"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"tool":"read_file","path":"output.txt"}"#,
            rejected_1.as_str(),
            // Round 2: Producer → Critic → Referee rejects → budget exhausted.
            r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
            r#"{"summary":"draft v2"}"#,
            r#"{"tool":"read_file","path":"output.txt"}"#,
            r#"{"status":"accepted","content":"review ok"}"#,
            r#"{"tool":"read_file","path":"output.txt"}"#,
            rejected_2.as_str(),
        ]);
        let runner = DeliberatingNodeRunner::new(&provider, &provider);
        let telemetry = crate::telemetry::VecTelemetry::new();
        let result = runner.run_node(
            work_request_with_artifact("do something", &temp),
            &telemetry,
        );
        let NodeRunResult::Failed(failure) = result else {
            panic!("[{dir}] expected Failed; got success or plan");
        };
        assert!(
            matches!(failure.recovery, RecoveryAction::Split { .. }),
            "[{dir}] semantic failure must produce Split recovery; got {:?}",
            failure.recovery
        );
        let records = telemetry.into_records();
        let classified = records.iter().find(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::FailureClassified { .. }
            )
        });
        assert!(
            classified.is_some(),
            "[{dir}] must emit FailureClassified telemetry"
        );
        if let Some(r) = classified {
            match &r.event {
                crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                    assert_eq!(recovery, "Split", "[{dir}]");
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn terminal_failure_produces_terminal_action() {
    let provider = ScriptedProvider::failing(ProviderErrorKind::Terminal, "invalid api key");
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(work_request("do something"), &telemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::Terminal { .. }),
        "fatal auth failure must produce Terminal recovery"
    );
    let records = telemetry.into_records();
    let classified = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::FailureClassified { .. }
        )
    });
    assert!(
        classified.is_some(),
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "Terminal");
            }
            _ => unreachable!(),
        }
    }
}
