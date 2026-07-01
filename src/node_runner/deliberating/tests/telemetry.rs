use super::*;

/// StateEntered/EventReceived/EffectEmitted records from a deliberation run
/// must carry the request's node_id and attempt, since a viewer needs them to
/// tell nodes and retries apart; other event kinds (e.g. RolePromptRendered)
/// must not be stamped, since node/attempt are already implied by context
/// there.
#[test]
fn deliberation_run_stamps_node_context_on_engine_events_only() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"work done"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let mut request = work_request("do the work");
    request.node_id = NodeId("root-child-0".to_string());
    request.attempt = 2;

    let _ = runner.run_node(request, &telemetry);

    let records = telemetry.into_records();
    let engine_records: Vec<_> = records
        .iter()
        .filter(|r| {
            matches!(
                r.event,
                crate::telemetry::TelemetryEvent::StateEntered { .. }
                    | crate::telemetry::TelemetryEvent::EventReceived { .. }
                    | crate::telemetry::TelemetryEvent::EffectEmitted { .. }
            )
        })
        .collect();
    assert!(
        !engine_records.is_empty(),
        "a completed deliberation run must emit at least one engine event"
    );
    for record in &engine_records {
        assert_eq!(
            record.node_id.as_deref(),
            Some("root-child-0"),
            "engine event {} must carry the request's node_id",
            record.event.kind_slug()
        );
        assert_eq!(
            record.attempt,
            Some(2),
            "engine event {} must carry the request's attempt",
            record.event.kind_slug()
        );
    }

    let role_prompt_rendered = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::RolePromptRendered { .. }
        )
    });
    if let Some(record) = role_prompt_rendered {
        assert_eq!(
            record.node_id, None,
            "role-layer events are not one of the enriched kinds and must stay unstamped"
        );
    }
}
