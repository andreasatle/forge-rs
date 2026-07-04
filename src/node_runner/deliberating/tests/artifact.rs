use super::*;
use crate::artifacts::WorkspaceFileOps;

#[test]
fn deliberating_artifact_work_writes_work_attempt_workspace() {
    let temp = TempDir::new("tool-artifact-update");

    // Producer: first call returns write_file, second returns accepted.
    // Critic and Referee must call read_file before accepting (enforcement).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"result.txt","content":"done"}"#,
        r#"{"summary":"I wrote result.txt"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"tool":"read_file","path":"hello.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = work_request_with_artifact("write a result file", &temp);
    let workspace = request
        .work_attempt
        .as_ref()
        .expect("artifact Work request must carry WorkAttempt")
        .workspace
        .clone();
    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::WorkAccepted(work_result) = result else {
        panic!("expected WorkAccepted");
    };
    assert_eq!(
        work_result.work.summary, "I wrote result.txt",
        "summary must be the accepted content, not the tool request"
    );
    let content = workspace
        .borrow()
        .read_file("result.txt")
        .expect("result.txt must be written into the WorkAttempt workspace");
    assert_eq!(content, "done");
}

#[test]
fn reviewer_can_read_work_attempt_target_file_with_relative_path() {
    let temp = TempDir::new("reviewer-work-attempt-target");
    let view = make_artifact_view(&temp, "main.py", "print('old')\n");
    let work_attempt = work_attempt_for_view(&view);
    let workspace = work_attempt.workspace.clone();

    let provider = ScriptedProvider::from_strs(&[
        r#"{"tool":"write_file","path":"main.py","content":"print('new')\n"}"#,
        r#"{"summary":"updated main.py"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"main.py contains the WorkAttempt update"}"#,
        r#"{"tool":"read_file","path":"main.py"}"#,
        r#"{"status":"accepted","content":"approved WorkAttempt main.py"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let request = NodeRunRequest {
        kind: NodeKind::Work,
        node_id: NodeId("test-node".to_string()),
        objective: "Update the program.".to_string(),
        target_files: vec!["main.py".to_string()],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        artifact_view: Some(view),
        worker_role: None,
        work_attempt: Some(work_attempt),
    };

    let result = runner.run_node(request, &NoopTelemetry);

    let NodeRunResult::WorkAccepted(_) = result else {
        panic!("expected WorkAccepted");
    };
    let content = workspace
        .borrow()
        .read_file("main.py")
        .expect("main.py must be readable from WorkAttempt workspace");
    assert_eq!(content, "print('new')\n");
}
