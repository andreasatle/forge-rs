use super::*;

// ── validation tests ──────────────────────────────────────────────────────

#[test]
fn validation_pass_allows_commit() {
    let (_temp, artifact) = fixture("validation-pass");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
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
        matches!(event, SchedulerEvent::IntegrationSucceeded { .. }),
        "AlwaysPassValidator must allow integration; got: {event:#?}"
    );

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_ne!(
        new_sha, original_sha,
        "commit must advance when validation passes"
    );
}

#[test]
fn validation_failure_blocks_commit() {
    let (_temp, artifact) = fixture("validation-fail");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact)
        .with_validator(Rc::new(AlwaysFailValidator));

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
        matches!(event, SchedulerEvent::IntegrationFailed { .. }),
        "failing validator must block integration; got: {event:#?}"
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "commit must not advance when validation fails"
    );

    let log_count = git_output(&repo_path, &["rev-list", "--count", "HEAD"]);
    assert_eq!(
        log_count, "1",
        "commit history must contain only the initial commit after validation failure"
    );
}

#[test]
fn retry_worker_receives_validation_diagnostics_and_can_fix_file() {
    let (_temp, artifact) = fixture("validation-retry-fixes-file");
    let repo_path = artifact.repo_path.clone();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let runner = FixOnValidationRetryRunner {
        requests: requests.clone(),
    };
    let handler =
        SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(MainPyValidator));
    let mut node = work_node("W", "make main.py valid");
    node.target_files = vec!["main.py".to_string()];
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![node],
            next_id: 0,
            id_seed: 0,
        },
        run_config: RunConfig::default(),
    };

    let output = run_scheduler(handler, state);

    let SchedulerTerminalOutput::Complete {
        graph,
        recovery_summary,
    } = output
    else {
        panic!("expected Complete after retry, got {output:#?}");
    };
    assert_eq!(recovery_summary.retry_count, 1);
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes[1].status, NodeStatus::Completed);

    let captured = requests.borrow();
    assert_eq!(captured.len(), 2, "worker must run twice");
    assert_eq!(captured[0].attempt, 0);
    assert_eq!(captured[1].attempt, 1);
    assert_eq!(captured[1].target_files, vec!["main.py"]);
    assert!(captured[1].objective.contains("make main.py valid"));
    assert!(captured[1].objective.contains("Target files: main.py"));
    assert!(
        captured[1]
            .objective
            .contains("command: custom-validator main.py")
    );
    assert!(captured[1].objective.contains("exit code: 7"));
    assert!(captured[1].objective.contains("first location: main.py:1"));
    assert!(captured[1].objective.contains("main.py:1: invalid syntax"));
    assert!(
        captured[1]
            .objective
            .contains("fix the existing file using file tools before accepting")
    );
    assert!(
        !captured[1].objective.contains("checked main.py"),
        "stdout should remain in telemetry, not the retry objective"
    );

    let final_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    let final_content = git_output(&repo_path, &["show", &format!("{final_sha}:main.py")]);
    assert_eq!(final_content, "ok");
}

#[test]
fn validation_failure_telemetry_keeps_full_diagnostics() {
    let (_temp, artifact) = fixture("validation-fail-telemetry");
    let telemetry = Rc::new(VecTelemetry::new());
    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact)
        .with_validator(Rc::new(AlwaysFailValidator))
        .with_telemetry(telemetry.clone());

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
    });

    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    let records = telemetry.records();
    let event = records
        .iter()
        .find_map(|record| match &record.event {
            TelemetryEvent::ValidationFailed {
                command,
                exit_code,
                stdout,
                stderr,
                ..
            } => Some((command, exit_code, stdout, stderr)),
            _ => None,
        })
        .expect("validation failure telemetry must be recorded");

    assert_eq!(event.0.as_deref(), Some("validator test command"));
    assert_eq!(*event.1, Some(1));
    assert_eq!(event.2.as_deref(), Some("validator stdout"));
    assert_eq!(event.3.as_deref(), Some("validator stderr"));
}

#[test]
fn validation_failure_records_attempt_evidence_before_cleanup() {
    let (_temp, artifact) = fixture("validation-fail-evidence");
    let telemetry = Rc::new(VecTelemetry::new());
    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello from failed attempt\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact)
        .with_validator(Rc::new(AlwaysFailValidator))
        .with_telemetry(telemetry.clone());

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
    });

    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    let records = telemetry.records();
    let evidence = records
        .iter()
        .find_map(|record| match &record.event {
            TelemetryEvent::WorkAttemptDiscarded {
                attempt_id,
                node_id,
                attempt,
                base_commit,
                changed_files,
                git_diff,
                reason,
            } => Some((
                attempt_id,
                node_id,
                attempt,
                base_commit,
                changed_files,
                git_diff,
                reason,
            )),
            _ => None,
        })
        .expect("discarded failed attempt evidence must be recorded");

    assert_eq!(evidence.0, "W:0");
    assert_eq!(evidence.1, "W");
    assert_eq!(*evidence.2, 0);
    assert!(!evidence.3.is_empty(), "base commit must be recorded");
    assert!(evidence.4.contains(&"output.txt".to_string()));
    assert!(evidence.5.contains("hello from failed attempt"));
    assert!(evidence.6.contains("validator stderr"));
}

#[test]
fn validator_runs_after_workspace_mutation() {
    let (_temp, artifact) = fixture("validator-after-workspace-mutation");

    let runner = FileWritingRunner {
        path: "applied.txt".to_string(),
        content: "applied content\n".to_string(),
    };

    let found = Rc::new(RefCell::new(false));
    let validator = FileExistsValidator {
        path: "applied.txt".to_string(),
        found: found.clone(),
    };

    let h = SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

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
    });

    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote applied.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert!(
        *found.borrow(),
        "validator must see applied.txt in the WorkAttempt workspace"
    );
}

#[test]
fn no_diff_fails_before_running_validator() {
    let (_temp, artifact) = fixture("no-update-no-validator");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact)
        .with_validator(Rc::new(PanicOnCallValidator));

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "do some work".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    });

    // StaticNodeRunner does not mutate the WorkAttempt, so integration fails
    // semantically before project validation.
    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "no file changes".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert!(
        matches!(
            event,
            SchedulerEvent::IntegrationFailed {
                failure: IntegrationFailure {
                    kind: FailureKind::WorkSemanticValidationFailure,
                    ..
                },
                ..
            }
        ),
        "no-diff integration must fail without calling validator; got: {event:#?}"
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "commit must not change when no artifact update was pending"
    );
}

#[test]
fn validation_pass_sets_validation_passed_true() {
    let (_temp, artifact) = fixture("vp-pass-true");

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
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
    });
    assert_eq!(
        h.validation_passed(),
        None,
        "validation_passed must be None before IntegrateWork"
    );

    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert_eq!(
        h.validation_passed(),
        Some(true),
        "validation_passed must be Some(true) after AlwaysPassValidator"
    );
}

#[test]
fn validation_failure_sets_validation_passed_false() {
    let (_temp, artifact) = fixture("vp-fail-false");

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact)
        .with_validator(Rc::new(AlwaysFailValidator));

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
    });

    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert_eq!(
        h.validation_passed(),
        Some(false),
        "validation_passed must be Some(false) after AlwaysFailValidator"
    );
}

#[test]
fn no_diff_leaves_validation_passed_none() {
    let (_temp, artifact) = fixture("vp-no-update-none");

    let h = SchedulerHandler::with_artifact(StaticNodeRunner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "do some work".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    });

    h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "no files".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert_eq!(
        h.validation_passed(),
        None,
        "validation_passed must remain None when no artifact update was pending"
    );
}

#[test]
fn validation_passed_true_even_when_integration_conflicts() {
    let (_temp, artifact) = fixture("vp-true-on-cas-conflict");
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
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
    });

    // Advance the branch externally so the integrate() CAS check fails.
    advance_branch_in_bare(&repo_path, "main");

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
        matches!(event, SchedulerEvent::IntegrationFailed { .. }),
        "CAS conflict must produce IntegrationFailed; got: {event:#?}"
    );
    assert_eq!(
        h.validation_passed(),
        Some(true),
        "validation_passed must be Some(true) even when CAS integration fails after validation"
    );
}

#[test]
fn timeout_blocks_commit() {
    use crate::validation::CommandValidator;
    use std::time::Duration;

    let (_temp, artifact) = fixture("timeout-blocks-commit");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    // Validator times out immediately — sleep 5 with a 1-second budget.
    let validator = CommandValidator::new(
        vec![crate::validation::CommandSpec {
            program: "sleep".to_string(),
            args: vec!["5".to_string()],
            when_files_present: vec![],
            scope: crate::validation::ValidationScope::Workspace,
        }],
        Duration::from_secs(1),
    );
    let h = SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

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
        matches!(event, SchedulerEvent::IntegrationFailed { .. }),
        "timed-out validator must block integration; got: {event:#?}"
    );

    let sha_after = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        sha_after, original_sha,
        "artifact commit must not change when validation times out"
    );
}
