use super::*;

#[test]
fn deliberating_work_result_includes_tool_artifact_update() {
    let temp = TempDir::new("tool-artifact-update");
    let view = make_artifact_view(&temp, "hello.txt", "world\n");

    // Producer: first call returns write_file, second returns accepted.
    // Critic and Referee must call read_file before accepting (enforcement).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        r#"{"status":"accepted","content":"I wrote result.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "write a result file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(
        work_result.work.summary, "I wrote result.txt",
        "summary must be the accepted content, not the tool request"
    );
    let update = work_result
        .artifact_update
        .expect("tool write_file must produce an artifact_update");
    assert_eq!(
        update.changes.len(),
        1,
        "must have exactly one pending change"
    );
    match &update.changes[0] {
        FileChange::Write { path, content } => {
            assert_eq!(path, "result.txt");
            assert_eq!(content, "done");
        }
        other => panic!("expected Write change from tool, got {other:?}"),
    }
}

#[test]
fn reviewer_can_read_staged_target_file_with_relative_path() {
    let temp = TempDir::new("reviewer-staged-target");
    let view = make_artifact_view(&temp, "main.py", "print('old')\n");

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"main.py","content":"print('new')\n"}"#,
        r#"{"status":"accepted","content":"updated main.py"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"main.py contains the staged update"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"approved staged main.py"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        objective: "Update the program.".to_string(),
        target_files: vec!["main.py".to_string()],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        work_attempt: None,
    };

    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::WorkAccepted(work) = result else {
        panic!("expected WorkAccepted");
    };
    let update = work
        .artifact_update
        .expect("producer write_file must produce artifact update");
    assert!(
        matches!(&update.changes[0], FileChange::Write { path, .. } if path == "main.py"),
        "staged target write must be preserved; got {:?}",
        update.changes
    );
}
