use super::*;

// ── artifact integration tests ────────────────────────────────────────────

/// Records the artifact view received on each `run_node` call. `Mutex`
/// (rather than `RefCell`) is required only so the type is `Sync`, as the
/// scheduler driver shares `&NodeRunner` across dispatch threads; every
/// test here runs with `dispatch_cap: 1` (via `RunConfig::default()`), so
/// at most one node is ever in flight.
struct ViewCapturingRunner {
    views: Arc<Mutex<Vec<Option<ArtifactView>>>>,
}

impl NodeRunner for ViewCapturingRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        self.views
            .lock()
            .expect("mutex poisoned")
            .push(request.artifact_view);
        NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: "captured".to_string(),
            },
        })
    }
}

/// On the first call writes a file; on the second call records the received view.
struct TwoStepRunner {
    call_count: Mutex<u32>,
    second_view: Arc<Mutex<Option<ArtifactView>>>,
}

impl NodeRunner for TwoStepRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let count = {
            let mut c = self.call_count.lock().expect("mutex poisoned");
            *c += 1;
            *c
        };
        match count {
            1 => {
                request
                    .work_attempt
                    .expect("work node must receive an attempt workspace")
                    .workspace
                    .lock()
                    .expect("workspace mutex poisoned")
                    .write_file("step1.txt", "written by node one\n")
                    .expect("test runner must write step1.txt");
                NodeRunResult::WorkAccepted(NodeRunWorkResult {
                    work: WorkOutput {
                        summary: "step one".to_string(),
                    },
                })
            }
            2 => {
                *self.second_view.lock().expect("mutex poisoned") = request.artifact_view;
                request
                    .work_attempt
                    .expect("work node must receive an attempt workspace")
                    .workspace
                    .lock()
                    .expect("workspace mutex poisoned")
                    .write_file("step2.txt", "written by node two\n")
                    .expect("test runner must write step2.txt");
                NodeRunResult::WorkAccepted(NodeRunWorkResult {
                    work: WorkOutput {
                        summary: "step two".to_string(),
                    },
                })
            }
            n => panic!("unexpected call count: {n}"),
        }
    }
}

struct DirtyThenRetryRunner {
    saw_clean_retry: Arc<Mutex<bool>>,
}

impl NodeRunner for DirtyThenRetryRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let attempt = request
            .work_attempt
            .expect("work node must receive an attempt workspace");
        let dirty_path = attempt
            .workspace
            .lock()
            .expect("workspace mutex poisoned")
            .path()
            .join("dirty.txt");
        if request.attempt == 0 {
            fs::write(&dirty_path, "failed attempt contents\n")
                .expect("failed to dirty attempt workspace");
            return NodeRunResult::Failed(NodeFailure {
                kind: FailureKind::ProviderFailure,
                message: "transient failure after dirtying worktree".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "retry after transient failure".to_string(),
                },
            });
        }

        *self.saw_clean_retry.lock().expect("mutex poisoned") = !dirty_path.exists();
        attempt
            .workspace
            .lock()
            .expect("workspace mutex poisoned")
            .write_file("clean.txt", "clean retry contents\n")
            .expect("retry attempt must write clean.txt");
        NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: "clean retry".to_string(),
            },
        })
    }
}

struct SchedulerScriptedProvider {
    responses: Mutex<std::collections::VecDeque<String>>,
}

impl SchedulerScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            responses: Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
        }
    }
}

impl crate::providers::ProviderClient for SchedulerScriptedProvider {
    fn call(
        &self,
        _request: crate::providers::ProviderRequest,
    ) -> Result<crate::providers::ProviderResponse, crate::providers::ProviderError> {
        let content = self
            .responses
            .lock()
            .expect("mutex poisoned")
            .pop_front()
            .expect("SchedulerScriptedProvider: responses exhausted");
        Ok(crate::providers::ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

#[test]
fn scheduler_handler_passes_artifact_view_to_node_runner() {
    let (_temp, artifact) = fixture("passes-view");
    let expected_sha = artifact.commit_sha.clone();

    let views = Arc::new(Mutex::new(Vec::new()));
    let runner = ViewCapturingRunner {
        views: views.clone(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("n1".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "do something".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    });

    let captured = views.lock().expect("mutex poisoned");
    assert_eq!(captured.len(), 1, "runner must be called exactly once");
    let view = captured[0]
        .as_ref()
        .expect("runner must receive Some(ArtifactView)");
    assert_eq!(
        view.commit_sha, expected_sha,
        "view must point at the artifact's current commit"
    );
}

#[test]
fn work_node_workspace_mutation_creates_new_commit() {
    let (_temp, artifact) = fixture("creates-commit");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello from work node\n".to_string(),
    };
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "write a file")],
        },
        run_config: RunConfig::default(),
    };
    run_scheduler(SchedulerHandler::with_artifact(runner, artifact), state);

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_ne!(
        new_sha, original_sha,
        "commit SHA must advance after an artifact update"
    );
}

/// The task manifest must be updated in the same commit that integrates a
/// Work node's changes, driven through the full RunNode → IntegrateWork
/// machine flow (not by constructing `IntegrateWork` directly).
#[test]
fn work_node_integration_records_task_manifest_in_same_commit() {
    let (_temp, artifact) = fixture("records-task-manifest");
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello from work node\n".to_string(),
    };
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "write a file")],
        },
        run_config: RunConfig::default(),
    };
    run_scheduler(SchedulerHandler::with_artifact(runner, artifact), state);

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);

    let manifest = git_output(
        &repo_path,
        &["show", &format!("{new_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["tasks"][0]["id"], "W");
    assert_eq!(manifest["tasks"][0]["objective"], "write a file");
    // The recorded commit is the pre-amend integration commit, not the final
    // amended HEAD that also carries the manifest update itself.
    assert_ne!(manifest["tasks"][0]["commit"], serde_json::Value::Null);
    assert_ne!(manifest["tasks"][0]["commit"], new_sha);

    let gitignore = git_output(&repo_path, &["show", &format!("{new_sha}:.gitignore")]);
    assert!(gitignore.lines().any(|line| line.trim() == ".forge/"));

    let file_content = git_output(&repo_path, &["show", &format!("{new_sha}:output.txt")]);
    assert_eq!(file_content, "hello from work node");
}

/// Two different teams' nodes spawned for the same manifest task (i.e. both
/// carrying the same `Node::task_id`, as `AfterTeams`-triggered nodes do) must
/// record manifest rows sharing that `id` — not each node's own id, which is
/// freshly minted per node and unstable across retries. Without this, a
/// downstream team's multi-team `AfterTeams` trigger could never join rows
/// across teams for "the same" task.
#[test]
fn two_teams_completing_the_same_task_id_share_the_manifest_row_id() {
    let (_temp, artifact) = fixture("shared-task-id-across-teams");
    let repo_path = artifact.repo_path.clone();

    let runner = TwoStepRunner {
        call_count: Mutex::new(0),
        second_view: Arc::new(Mutex::new(None)),
    };

    let mut node_a = work_node_with_deps("A", "team-a does its part", &[]);
    node_a.team = "team-a".to_string();
    node_a.task_id = Some("shared-task".to_string());

    let mut node_b = work_node_with_deps("B", "team-b does its part", &["A"]);
    node_b.team = "team-b".to_string();
    node_b.task_id = Some("shared-task".to_string());

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![node_a, node_b],
        },
        run_config: RunConfig::default(),
    };
    run_scheduler(SchedulerHandler::with_artifact(runner, artifact), state);

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let manifest = git_output(
        &repo_path,
        &["show", &format!("{new_sha}:.forge/tasks.json")],
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let tasks = manifest["tasks"]
        .as_array()
        .expect("manifest tasks must be an array");
    assert_eq!(tasks.len(), 2, "both team nodes must record a manifest row");

    let ids: Vec<&str> = tasks.iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids[0], ids[1],
        "rows for the same manifest task must share `id` across teams; got {ids:?}"
    );
    assert_eq!(ids[0], "shared-task");

    let teams: Vec<&str> = tasks.iter().map(|t| t["team"].as_str().unwrap()).collect();
    assert_eq!(teams, vec!["team-a", "team-b"]);
}

#[test]
fn second_work_node_sees_first_work_node_changes() {
    let (_temp, artifact) = fixture("second-sees-first");

    let second_view = Arc::new(Mutex::new(None));
    let runner = TwoStepRunner {
        call_count: Mutex::new(0),
        second_view: second_view.clone(),
    };

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![
                work_node_with_deps("A", "write the file", &[]),
                work_node_with_deps("B", "read the file", &["A"]),
            ],
        },
        run_config: RunConfig::default(),
    };
    run_scheduler(SchedulerHandler::with_artifact(runner, artifact), state);

    let view = second_view.lock().expect("mutex poisoned");
    let view = view
        .as_ref()
        .expect("node B must receive Some(ArtifactView)");
    let content = view
        .read_file("step1.txt")
        .expect("step1.txt must be visible to node B via its ArtifactView");
    assert_eq!(
        content, "written by node one\n",
        "node B must see the file written by node A"
    );
}

#[test]
fn work_node_without_update_preserves_commit() {
    let (_temp, artifact) = fixture("no-update-preserves");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "do some work")],
        },
        run_config: RunConfig::default(),
    };
    let output = run_scheduler(
        SchedulerHandler::with_artifact(StaticNodeRunner, artifact),
        state,
    );
    assert!(
        matches!(output, SchedulerTerminalOutput::Failed { .. }),
        "no-diff artifact Work must fail semantically; got {output:#?}"
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "commit SHA must not change when the runner produces no artifact update"
    );
}

#[test]
fn rejected_work_attempt_records_evidence_and_retry_starts_clean() {
    let (_temp, artifact) = fixture("rejected-evidence-clean-retry");
    let telemetry = Arc::new(VecTelemetry::new());
    let saw_clean_retry = Arc::new(Mutex::new(false));
    let runner = DirtyThenRetryRunner {
        saw_clean_retry: saw_clean_retry.clone(),
    };
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "dirty then retry")],
        },
        run_config: RunConfig::default(),
    };

    let output = run_scheduler(
        SchedulerHandler::with_artifact(runner, artifact).with_telemetry(telemetry.clone()),
        state,
    );

    let SchedulerTerminalOutput::Complete { graph, .. } = output else {
        panic!("expected retry to complete, got {output:#?}");
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes[1].status, NodeStatus::Completed);
    assert!(
        *saw_clean_retry.lock().expect("mutex poisoned"),
        "retry must start from a clean worktree"
    );

    let records = telemetry.records();
    let evidence = records
        .iter()
        .find_map(|record| match &record.event {
            TelemetryEvent::WorkAttemptDiscarded {
                attempt_id,
                changed_files,
                git_diff,
                reason,
                ..
            } => Some((attempt_id, changed_files, git_diff, reason)),
            _ => None,
        })
        .expect("rejected attempt evidence must be recorded before cleanup");

    assert_eq!(evidence.0, "W:0");
    assert!(evidence.1.contains(&"dirty.txt".to_string()));
    assert!(evidence.2.contains("failed attempt contents"));
    assert_eq!(evidence.3, "transient failure after dirtying worktree");
}

#[test]
fn revision_exhaustion_records_final_work_attempt_evidence_before_cleanup() {
    let (_temp, artifact) = fixture("revision-exhaustion-evidence");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();
    let telemetry = Arc::new(VecTelemetry::new());
    let provider = SchedulerScriptedProvider::from_strs(&[
        // Round 1: write v1, review it, and request a revision.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"summary":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"reviewed draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"needs a stronger ending"}"#,
        // Round 2: revise the same WorkAttempt workspace, then reject terminally.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"summary":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"reviewed draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"still not acceptable"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let mut node = work_node("W", "revise until the referee rejects");
    // Force scheduler recovery exhaustion so the final referee rejection remains
    // terminal at the scheduler boundary while still exercising one node run.
    node.attempt = 3;
    let state = SchedulerState::Active {
        graph: RunGraph { nodes: vec![node] },
        run_config: RunConfig::default(),
    };

    let output = run_scheduler(
        SchedulerHandler::with_artifact(runner, artifact).with_telemetry(telemetry.clone()),
        state,
    );

    let SchedulerTerminalOutput::Failed { graph, reason } = output else {
        panic!("expected terminal scheduler failure after revision exhaustion");
    };
    let FailureReason::AttemptsExhausted { node_id, .. } = reason else {
        panic!("expected AttemptsExhausted, got {reason:?}");
    };
    assert_eq!(node_id, "W", "exhausted node id");
    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes[0].attempt, 3);
    assert_eq!(
        git_output(&repo_path, &["rev-parse", "HEAD"]),
        original_sha,
        "terminally rejected WorkAttempt must not advance the artifact commit"
    );

    let records = telemetry.records();
    let evidence = records
        .iter()
        .find_map(|record| match &record.event {
            TelemetryEvent::WorkAttemptDiscarded {
                attempt_id,
                changed_files,
                git_diff,
                reason,
                ..
            } => Some((attempt_id, changed_files, git_diff, reason)),
            _ => None,
        })
        .expect("terminally rejected revision attempt must record evidence before cleanup");

    assert_eq!(evidence.0, "W:3");
    assert!(evidence.1.contains(&"output.txt".to_string()));
    assert!(
        evidence.2.contains("draft v2"),
        "evidence diff must include the final revised workspace state; got:\n{}",
        evidence.2
    );
    assert!(
        !evidence.2.contains("draft v1"),
        "evidence diff should reflect the final accumulated workspace state, not the overwritten draft; got:\n{}",
        evidence.2
    );
    assert!(
        evidence.3.contains("revision limit exhausted")
            && evidence.3.contains("still not acceptable"),
        "evidence reason must preserve terminal referee rejection; got: {}",
        evidence.3
    );
}

#[test]
fn deliberation_revision_stays_inside_single_scheduler_attempt_until_acceptance() {
    let (_temp, artifact) = fixture("revision-single-attempt");
    let repo_path = artifact.repo_path.clone();
    let provider = SchedulerScriptedProvider::from_strs(&[
        // Round 1: write v1, then Referee requests an internal deliberation revision.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v1\n"}"#,
        r#"{"summary":"draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"reviewed draft v1"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"rejected","reason":"needs revision"}"#,
        // Round 2: revise and accept without creating a scheduler retry node.
        r#"{"tool":"write_file","path":"output.txt","content":"draft v2\n"}"#,
        r#"{"summary":"draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"reviewed draft v2"}"#,
        r#"{"tool":"read_file","path":"output.txt"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "revise then accept")],
        },
        run_config: RunConfig::default(),
    };

    let output = run_scheduler(SchedulerHandler::with_artifact(runner, artifact), state);

    let SchedulerTerminalOutput::Complete {
        graph,
        recovery_summary,
    } = output
    else {
        panic!("expected revised Work node to complete in one scheduler attempt");
    };
    assert!(
        !recovery_summary.recovered,
        "internal deliberation revision must not count as scheduler recovery"
    );
    assert_eq!(
        graph.nodes.len(),
        1,
        "internal deliberation revision must not create retry or elevated nodes"
    );
    let node = &graph.nodes[0];
    assert_eq!(node.status, NodeStatus::Completed);
    assert_eq!(node.attempt, 0);
    assert!(matches!(node.origin, NodeOrigin::Root));
    assert_eq!(node.summary.as_deref(), Some("draft v2"));

    let head = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let output_content = git_output(&repo_path, &["show", &format!("{head}:output.txt")]);
    assert_eq!(
        output_content, "draft v2",
        "integrated artifact must contain the revised workspace state"
    );
}

// ── handler boundary tests ─────────────────────────────────────────────────

#[test]
fn run_node_does_not_commit_workspace_mutation() {
    let (_temp, artifact) = fixture("no-commit-on-run");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello from work node\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "write a file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    });

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "RunNode must not commit; artifact mutation must happen only during IntegrateWork"
    );
}

#[test]
fn integrate_work_commits_pending_workspace_mutation() {
    let (_temp, artifact) = fixture("commit-on-integrate");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello from work node\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "write a file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
        team: String::new(),
        adapter: String::new(),
        northstar: String::new(),
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        objective: "integration test objective".to_string(),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
        team: "test-team".to_string(),
        task_id: None,
    });

    assert!(
        matches!(event, SchedulerEvent::IntegrationSucceeded { .. }),
        "IntegrateWork must return Succeeded; got: {event:#?}"
    );

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_ne!(
        new_sha, original_sha,
        "IntegrateWork must advance the artifact commit"
    );

    let output_content = git_output(&repo_path, &["show", &format!("{new_sha}:output.txt")]);
    assert_eq!(
        output_content, "hello from work node",
        "output.txt must exist in the integrated commit"
    );

    // The handler's artifact now reflects the new commit.
    let current_sha = h.artifact().expect("artifact must be present").commit_sha;
    assert_eq!(
        current_sha, new_sha,
        "handler artifact must point at the integrated commit"
    );
}
