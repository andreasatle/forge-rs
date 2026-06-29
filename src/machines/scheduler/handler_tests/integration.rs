use super::*;

// ── artifact integration tests ────────────────────────────────────────────

/// Records the artifact view received on each `run_node` call.
struct ViewCapturingRunner {
    views: Rc<RefCell<Vec<Option<ArtifactView>>>>,
}

impl NodeRunner for ViewCapturingRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        self.views.borrow_mut().push(request.artifact_view);
        NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: "captured".to_string(),
            },
        })
    }
}

/// On the first call writes a file; on the second call records the received view.
struct TwoStepRunner {
    call_count: RefCell<u32>,
    second_view: Rc<RefCell<Option<ArtifactView>>>,
}

impl NodeRunner for TwoStepRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let count = {
            let mut c = self.call_count.borrow_mut();
            *c += 1;
            *c
        };
        match count {
            1 => {
                request
                    .work_attempt
                    .expect("work node must receive an attempt workspace")
                    .workspace
                    .borrow_mut()
                    .write_file("step1.txt", "written by node one\n")
                    .expect("test runner must write step1.txt");
                NodeRunResult::WorkAccepted(NodeRunWorkResult {
                    work: WorkOutput {
                        summary: "step one".to_string(),
                    },
                })
            }
            2 => {
                *self.second_view.borrow_mut() = request.artifact_view;
                request
                    .work_attempt
                    .expect("work node must receive an attempt workspace")
                    .workspace
                    .borrow_mut()
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
    saw_clean_retry: Rc<RefCell<bool>>,
}

impl NodeRunner for DirtyThenRetryRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let attempt = request
            .work_attempt
            .expect("work node must receive an attempt workspace");
        let dirty_path = attempt.workspace.borrow().path().join("dirty.txt");
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

        *self.saw_clean_retry.borrow_mut() = !dirty_path.exists();
        attempt
            .workspace
            .borrow_mut()
            .write_file("clean.txt", "clean retry contents\n")
            .expect("retry attempt must write clean.txt");
        NodeRunResult::WorkAccepted(NodeRunWorkResult {
            work: WorkOutput {
                summary: "clean retry".to_string(),
            },
        })
    }
}

#[test]
fn scheduler_handler_passes_artifact_view_to_node_runner() {
    let (_temp, artifact) = fixture("passes-view");
    let expected_sha = artifact.commit_sha.clone();

    let views = Rc::new(RefCell::new(Vec::new()));
    let runner = ViewCapturingRunner {
        views: views.clone(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("n1".to_string()),
        kind: NodeKind::Work,
        objective: "do something".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });

    let captured = views.borrow();
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
    let state = SchedulerState::Running {
        graph: RunGraph {
            nodes: vec![work_node("W", "write a file")],
            next_id: 0,
        },
    };
    run_machine(SchedulerHandler::with_artifact(runner, artifact), state);

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_ne!(
        new_sha, original_sha,
        "commit SHA must advance after an artifact update"
    );
}

#[test]
fn second_work_node_sees_first_work_node_changes() {
    let (_temp, artifact) = fixture("second-sees-first");

    let second_view = Rc::new(RefCell::new(None));
    let runner = TwoStepRunner {
        call_count: RefCell::new(0),
        second_view: second_view.clone(),
    };

    let state = SchedulerState::Running {
        graph: RunGraph {
            nodes: vec![
                work_node_with_deps("A", "write the file", &[]),
                work_node_with_deps("B", "read the file", &["A"]),
            ],
            next_id: 0,
        },
    };
    run_machine(SchedulerHandler::with_artifact(runner, artifact), state);

    let view = second_view.borrow();
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

    let state = SchedulerState::Running {
        graph: RunGraph {
            nodes: vec![work_node("W", "do some work")],
            next_id: 0,
        },
    };
    let output = run_machine(
        SchedulerHandler::with_artifact(StaticNodeRunner, artifact),
        state,
    );
    assert!(
        matches!(output, SchedulerOutput::Failed { .. }),
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
    let telemetry = Rc::new(VecTelemetry::new());
    let saw_clean_retry = Rc::new(RefCell::new(false));
    let runner = DirtyThenRetryRunner {
        saw_clean_retry: saw_clean_retry.clone(),
    };
    let state = SchedulerState::Running {
        graph: RunGraph {
            nodes: vec![work_node("W", "dirty then retry")],
            next_id: 0,
        },
    };

    let output = run_machine(
        SchedulerHandler::with_artifact(runner, artifact).with_telemetry(telemetry.clone()),
        state,
    );

    let SchedulerOutput::Complete { graph, .. } = output else {
        panic!("expected retry to complete, got {output:#?}");
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes[1].status, NodeStatus::Completed);
    assert!(
        *saw_clean_retry.borrow(),
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
        kind: NodeKind::Work,
        objective: "write a file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
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
        kind: NodeKind::Work,
        objective: "write a file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert!(
        matches!(
            event,
            SchedulerEvent::IntegrationReturned {
                outcome: IntegrationOutcome::Succeeded(_),
                ..
            }
        ),
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
}

#[test]
fn second_work_node_sees_first_only_after_integration() {
    let (_temp, artifact) = fixture("second-sees-after-integration");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let writer = FileWritingRunner {
        path: "step1.txt".to_string(),
        content: "written by node one\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(writer, artifact);

    // RunNode for A: stores the update but does NOT commit.
    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("A".to_string()),
        kind: NodeKind::Work,
        objective: "write the file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });
    let sha_before_integrate = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_before_integrate, original_sha,
        "commit must not advance before IntegrateWork"
    );

    // IntegrateWork for A: applies the update and commits.
    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("A".to_string()),
        work: WorkOutput {
            summary: "wrote step1.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });
    let sha_after_integrate = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_ne!(
        sha_after_integrate, original_sha,
        "commit must advance after IntegrateWork"
    );

    // The handler's artifact now reflects the new commit.
    let current_sha = h.artifact().expect("artifact must be present").commit_sha;
    assert_eq!(
        current_sha, sha_after_integrate,
        "handler artifact must point at the integrated commit"
    );
}

#[test]
fn work_node_without_update_fails_without_commit() {
    let (_temp, artifact) = fixture("no-update-no-commit");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        kind: NodeKind::Work,
        objective: "do some work".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "completed".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert!(
        matches!(
            event,
            SchedulerEvent::IntegrationReturned {
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::WorkSemanticValidationFailure,
                    ..
                }),
                ..
            }
        ),
        "IntegrateWork with no workspace diff must fail semantically"
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "commit must not change when no artifact update was pending"
    );
}
