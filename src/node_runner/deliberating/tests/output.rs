use super::*;

#[test]
fn deliberating_runner_plan_returns_plan_output() {
    let tasks_json = r#"{"tasks":[{"id":"task-1","objective":"the actual work","operation":"modify","targets":["work.txt"],"depends_on":[]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(plan_request("plan the work"), &NoopTelemetry);
    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted");
    };
    assert_eq!(plan.children.len(), 1);
    assert_eq!(plan.children[0].kind, NodeKind::Work);
    assert_eq!(plan.children[0].objective, "the actual work");
    assert_eq!(plan.children[0].target_files, vec!["work.txt".to_string()]);
}

#[test]
fn deliberating_runner_work_returns_work_output() {
    let temp = TempDir::new("work-output");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"finished\n"}"#,
        r#"{"status":"accepted","content":"finished the task"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        work_request_with_artifact("write some code", &temp),
        &NoopTelemetry,
    );
    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(work_result.work.summary, "finished the task");
}

#[test]
fn failed_deliberation_with_invalid_update_does_not_return_work_output() {
    let update = crate::artifacts::ArtifactUpdate {
        changes: vec![FileChange::Replace {
            path: "main.py".to_string(),
            old: "missing target".to_string(),
            new: "replacement".to_string(),
        }],
    };
    let result = super::super::output::map_output(
        crate::machines::deliberation::DeliberationTerminalOutput::Failed {
            kind: FailureKind::WorkSemanticValidationFailure,
            reason: "artifact update could not be applied to the staged view".to_string(),
        },
        NodeKind::Work,
        Some(update),
        &NoopTelemetry,
    );

    assert!(
        matches!(
            result,
            NodeRunResult::Failed(crate::machines::scheduler::NodeFailure {
                kind: FailureKind::WorkSemanticValidationFailure,
                ..
            })
        ),
        "failed deliberation must not expose an invalid WorkOutput to integration"
    );
}

#[test]
fn deliberating_runner_revision_uses_latest_producer_content() {
    let temp = TempDir::new("revision-latest");
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"status":"accepted","content":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review done"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"needs work"}"#,
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"status":"accepted","content":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"review ok"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(
        work_request_with_artifact("refine the plan", &temp),
        &NoopTelemetry,
    );
    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(work_result.work.summary, "draft v2");
}

#[test]
fn non_artifact_worker_without_tool_update_succeeds_without_artifact_update() {
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"output content"}"#,
        r#"{"status":"accepted","content":"output content"}"#,
        r#"{"status":"accepted","content":"output content"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let result = runner.run_node(work_request("produce some output"), &NoopTelemetry);
    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(work_result.work.summary, "output content");
    assert!(
        work_result.artifact_update.is_none(),
        "explicit non-artifact Work must not synthesize an artifact update"
    );
}

#[test]
fn prose_planner_content_triggers_retry_and_fails() {
    // Step 2: prose content is no longer silently accepted as a single work node.
    // The runner validates the accepted content as PlannerOutput and retries.
    // After MAX_PROTOCOL_RETRIES the plan node returns Failed.
    let provider = ScriptedProvider::from_strs(&[
        r#"{"status":"accepted","content":"Just do the work however you see fit."}"#,
        r#"{"status":"accepted","content":"Still prose, not JSON."}"#,
        r#"{"status":"accepted","content":"Also prose."}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(plan_request("plan the work"), &telemetry);

    let NodeRunResult::Failed(_) = result else {
        panic!("expected Failed after prose planner content exhausts retries");
    };

    // PlannerOutputFallback must NOT be emitted: validation fails in the runner before
    // map_plan_output is reached.
    let records = telemetry.into_records();
    let has_fallback = records.iter().any(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::PlannerOutputFallback
        )
    });
    assert!(
        !has_fallback,
        "PlannerOutputFallback must not be emitted when runner validation fails first"
    );
}

#[test]
fn structured_planner_output_creates_multiple_work_nodes() {
    let tasks_json = r#"{"tasks":[{"id":"alpha","objective":"do alpha","operation":"modify","targets":["alpha.txt"],"depends_on":[]},{"id":"beta","objective":"do beta","operation":"modify","targets":["beta.txt"],"depends_on":["alpha"]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(plan_request("plan the work"), &telemetry);

    let NodeRunResult::PlanAccepted(plan) = result else {
        panic!("expected PlanAccepted");
    };
    assert_eq!(plan.children.len(), 2, "must produce two work nodes");
    assert_eq!(plan.children[0].id, NodeId("alpha".to_string()));
    assert_eq!(plan.children[0].objective, "do alpha");
    assert_eq!(plan.children[0].target_files, vec!["alpha.txt".to_string()]);
    assert!(plan.children[0].dependencies.is_empty());
    assert_eq!(plan.children[1].id, NodeId("beta".to_string()));
    assert_eq!(plan.children[1].objective, "do beta");
    assert_eq!(plan.children[1].target_files, vec!["beta.txt".to_string()]);
    assert_eq!(
        plan.children[1].dependencies,
        vec![NodeId("alpha".to_string())]
    );

    let records = telemetry.into_records();
    let parsed = records.iter().find(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::PlannerOutputParsed { .. }
        )
    });
    assert!(parsed.is_some(), "must emit PlannerOutputParsed telemetry");
    if let Some(r) = parsed {
        match &r.event {
            crate::telemetry::TelemetryEvent::PlannerOutputParsed {
                task_count,
                dependency_count,
            } => {
                assert_eq!(*task_count, 2);
                assert_eq!(*dependency_count, 1);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn invalid_structured_plan_returns_failed() {
    // Parses as PlannerOutput but has a self-dependency — validation must fail loudly.
    // All three producer attempts return the same invalid plan, exhausting retries.
    let tasks_json = r#"{"tasks":[{"id":"x","objective":"do x","operation":"modify","targets":["x.txt"],"depends_on":["x"]}]}"#;
    let provider = ScriptedProvider::from_strs(&[
        tasks_json, // Producer attempt 1
        tasks_json, // Producer attempt 2 (retry)
        tasks_json, // Producer attempt 3 (retry)
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let telemetry = crate::telemetry::VecTelemetry::new();
    let result = runner.run_node(plan_request("plan the work"), &telemetry);

    let NodeRunResult::Failed(failure) = result else {
        panic!("expected Failed for invalid structured plan");
    };
    assert!(
        failure.message.contains("self-dependency"),
        "failure reason must describe the validation error; got: {}",
        failure.message
    );
    assert!(matches!(failure.recovery, RecoveryAction::Terminal { .. }));

    // Step 2: validation failure is now recorded as ParseFailed in the runner layer.
    let records = telemetry.into_records();
    let has_parse_failed = records.iter().any(|r| {
        matches!(
            r.event,
            crate::telemetry::TelemetryEvent::ParseFailed { .. }
        )
    });
    assert!(
        has_parse_failed,
        "must emit ParseFailed telemetry for planner validation failure"
    );
}
