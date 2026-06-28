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
            artifact_update: None,
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
            1 => NodeRunResult::WorkAccepted(NodeRunWorkResult {
                work: WorkOutput {
                    summary: "step one".to_string(),
                },
                artifact_update: Some(ArtifactUpdate {
                    changes: vec![FileChange::Write {
                        path: "step1.txt".to_string(),
                        content: "written by node one\n".to_string(),
                    }],
                }),
            }),
            2 => {
                *self.second_view.borrow_mut() = request.artifact_view;
                NodeRunResult::WorkAccepted(NodeRunWorkResult {
                    work: WorkOutput {
                        summary: "step two".to_string(),
                    },
                    artifact_update: None,
                })
            }
            n => panic!("unexpected call count: {n}"),
        }
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
fn work_node_artifact_update_creates_new_commit() {
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
    run_machine(
        SchedulerHandler::with_artifact(StaticNodeRunner, artifact),
        state,
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "commit SHA must not change when the runner produces no artifact update"
    );
}

// ── handler boundary tests ─────────────────────────────────────────────────

#[test]
fn run_node_does_not_commit_artifact_update() {
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
fn integrate_work_commits_pending_artifact_update() {
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
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
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
fn artifact_update_apply_failure_returns_integration_failure() {
    let (_temp, artifact) = fixture("apply-fail");
    let h = SchedulerHandler::with_artifact(BadReplaceRunner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        kind: NodeKind::Work,
        objective: "do work".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "done".to_string(),
        },
    });

    assert!(
        matches!(
            event,
            SchedulerEvent::IntegrationReturned {
                outcome: IntegrationOutcome::Failed(_),
                ..
            }
        ),
        "IntegrateWork must return Failed when apply errors; got: {event:#?}"
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
fn work_node_without_update_integrates_without_commit() {
    let (_temp, artifact) = fixture("no-update-no-commit");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        kind: NodeKind::Work,
        objective: "do some work".to_string(),
        target_files: vec![],
        model_tier: ModelTier::Cheap,
        attempt: 0,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "completed".to_string(),
        },
    });

    assert!(
        matches!(
            event,
            SchedulerEvent::IntegrationReturned {
                outcome: IntegrationOutcome::Succeeded(_),
                ..
            }
        ),
        "IntegrateWork with no pending update must return Succeeded"
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "commit must not change when no artifact update was pending"
    );
}
