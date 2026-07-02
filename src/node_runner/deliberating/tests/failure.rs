use super::*;

#[test]
fn deliberating_runner_provider_failure_returns_failed() {
    let provider = ScriptedProvider::failing(
        ProviderErrorKind::Retryable,
        "connection refused on http://localhost:8080/completion",
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("do something"), &NoopTelemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        failure
            .message
            .contains("provider error (Retryable): connection refused")
    );
    assert!(matches!(failure.recovery, RecoveryAction::Retry { .. }));
}

#[test]
fn deliberating_runner_preserves_deliberation_failure_reason() {
    let provider = ScriptedProvider::failing(
        ProviderErrorKind::Retryable,
        "connection refused on http://localhost:8080/completion",
    );
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("do something"), &NoopTelemetry);
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        failure
            .message
            .contains("provider error (Retryable): connection refused")
    );
    let RecoveryAction::Retry { message } = failure.recovery else {
        panic!("expected retry recovery for retryable provider error");
    };
    assert!(
        message.contains("provider error (Retryable): connection refused"),
        "retry message must include the original reason; got: {message}"
    );
}

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
        matches!(failure.recovery, RecoveryAction::Retry { .. }),
        "retryable provider error must produce Retry recovery"
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
fn semantic_failure_produces_elevate_action() {
    // Revision limit exhaustion is a semantic failure. The runner allows 1 revision,
    // so both Referee rejections are needed to exhaust the budget and produce
    // "revision limit exhausted: ..." → ElevateModel.
    let temp = TempDir::new("semantic-elevate");
    let provider = ScriptedProvider::from_strs(&[
        // Round 1: Producer → Critic → Referee rejects → revision loop.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"summary":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"needs improvement"}"#,
        // Round 2: Producer → Critic → Referee rejects → budget exhausted.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"summary":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"still not good enough"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        work_request_with_artifact("do something", &temp),
        &telemetry,
    );
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed; got success or plan");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::ElevateModel { .. }),
        "semantic failure must produce ElevateModel recovery; got {:?}",
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
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "ElevateModel");
            }
            _ => unreachable!(),
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

#[test]
fn deliberation_failure_produces_elevate_action_independent_of_message_text() {
    // Referee rejects with "task too large" twice, exhausting the revision budget.
    // The failure reason is "revision limit exhausted: task too large".
    // The classifier checks task-shape signals (Split) before revision-exhaustion
    // (ElevateModel), so "task too large" wins and maps to Split.
    let temp = TempDir::new("deliberation-elevate");
    let provider = ScriptedProvider::from_strs(&[
        // Round 1: Producer → Critic → Referee rejects "task too large" → revision loop.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"summary":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"task too large"}"#,
        // Round 2: Producer → Critic → Referee rejects "task too large" → budget exhausted.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"summary":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"task too large"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(
        work_request_with_artifact("do something", &temp),
        &telemetry,
    );
    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed");
    };
    assert!(
        matches!(failure.recovery, RecoveryAction::ElevateModel { .. }),
        "typed deliberation failure must produce ElevateModel recovery; got {:?}",
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
        "must emit FailureClassified telemetry"
    );
    if let Some(r) = classified {
        match &r.event {
            crate::telemetry::TelemetryEvent::FailureClassified { recovery, .. } => {
                assert_eq!(recovery, "ElevateModel");
            }
            _ => unreachable!(),
        }
    }
}
