use super::*;

#[test]
fn protocol_retry_records_role_layer_telemetry() {
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let provider = ScriptedProvider::from_strs(&["invalid text", r#"{"summary":"recovered"}"#]);
    let runner = ProviderRoleRunner::new(&provider);
    let telemetry = VecTelemetry::new();

    runner.run_role(producer_request("recover output"), &telemetry);

    let records = telemetry.records();
    assert!(records.iter().all(|record| record.source == "RoleMachine"));
    // "invalid text" starts with 'i', not '{', so preamble check fires.
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::RolePromptRendered {
            attempt_count: 2,
            prompt,
        } if prompt.contains("preamble text is not permitted")
    )));
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::ProviderResponseReceived {
            attempt_count: 1,
            raw_response,
        } if raw_response == "invalid text"
    )));
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::ParseFailed { parse_error, .. }
            if parse_error.contains("preamble text is not permitted")
    )));
    assert!(records.iter().any(|record| matches!(
        &record.event,
        TelemetryEvent::ProtocolRetry {
            attempt_count: 2,
            ..
        }
    )));
    assert!(records.iter().any(|record| matches!(
        record.event,
        TelemetryEvent::ParseSucceeded { attempt_count: 2 }
    )));
}

#[test]
fn file_telemetry_records_role_machine_source_and_event_identity() {
    use crate::telemetry::FileTelemetry;

    let temp = TempDir::new("role-machine-source-events");
    let telemetry = FileTelemetry::new(temp.0.clone());
    let provider = ScriptedProvider::from_strs(&["invalid text", r#"{"summary":"recovered"}"#]);
    let runner = ProviderRoleRunner::new(&provider);

    runner.run_role(producer_request("recover output"), &telemetry);

    let entries: Vec<String> = std::fs::read_dir(&temp.0)
        .expect("file telemetry directory must be readable")
        .map(|entry| {
            let path = entry.expect("telemetry entry must be readable").path();
            std::fs::read_to_string(path).expect("telemetry file must be readable")
        })
        .collect();
    assert!(
        entries
            .iter()
            .any(|content| content.contains("source: RoleMachine")
                && content.contains("subsource: Producer")
                && content.contains("kind: RolePromptRendered")),
        "file telemetry must preserve role prompt source, subsource, and event identity"
    );
    assert!(
        entries
            .iter()
            .any(|content| content.contains("source: RoleMachine")
                && content.contains("subsource: Producer")
                && content.contains("kind: ParseFailed")),
        "file telemetry must preserve parse-failure source, subsource, and event identity"
    );
    assert!(
        entries
            .iter()
            .any(|content| content.contains("source: RoleMachine")
                && content.contains("subsource: Producer")
                && content.contains("kind: ProtocolRetry")),
        "file telemetry must preserve protocol-retry source, subsource, and event identity"
    );
}

// --- git helpers for tool tests that need a real ArtifactView ---
