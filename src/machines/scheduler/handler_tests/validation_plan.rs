use super::*;
use crate::validation::{ValidationPlan, ValidationScope, ValidationStage, ValidationStep};

fn passing_plan() -> ValidationPlan {
    ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["true".to_string()],
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 30,
    }
}

fn failing_plan() -> ValidationPlan {
    ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["false".to_string()],
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 30,
    }
}

// ── integration executes node's plan ─────────────────────────────────────────

#[test]
fn integration_executes_nodes_passing_validation_plan() {
    // Invariant: when IntegrateWork carries a ValidationPlan, the plan's steps
    // are executed and — when all pass — integration succeeds.
    let (_temp, artifact) = fixture("vplan-passing");
    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    // Use PanicOnCallValidator: integration must NOT fall back to the global
    // validator when a ValidationPlan is present.
    let h = SchedulerHandler::with_artifact(runner, artifact)
        .with_validator(Rc::new(PanicOnCallValidator));

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
        objective: "integration test objective".to_string(),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: Some(passing_plan()),
    });

    assert!(
        matches!(event, SchedulerEvent::IntegrationSucceeded { .. }),
        "passing ValidationPlan must allow integration to succeed; got: {event:#?}"
    );
}

#[test]
fn integration_executes_nodes_failing_validation_plan() {
    // Invariant: when IntegrateWork carries a ValidationPlan whose step fails,
    // integration returns a failed outcome without committing.
    let (_temp, artifact) = fixture("vplan-failing");
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
        objective: "integration test objective".to_string(),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: Some(failing_plan()),
    });

    assert!(
        matches!(event, SchedulerEvent::IntegrationFailed { .. }),
        "failing ValidationPlan must block integration; got: {event:#?}"
    );
    let current_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        current_sha, original_sha,
        "artifact commit must not advance when validation plan fails"
    );
}

#[test]
fn integration_executes_validation_plan_with_target_file_scope() {
    // Invariant: integration passes the node's structured target files into
    // the node-owned ValidationPlan, and target-scoped steps append them.
    let (_temp, artifact) = fixture("vplan-target-scope");
    let runner = FileWritingRunner {
        path: "scoped.py".to_string(),
        content: "print('ok')\n".to_string(),
    };
    let plan = ValidationPlan {
        steps: vec![ValidationStep {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "test \"$1\" = scoped.py".to_string(),
                "scope-check".to_string(),
            ],
            when_artifacts_present: vec![],
            scope: ValidationScope::TargetFiles,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 30,
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "write scoped file".to_string(),
        target_files: vec!["scoped.py".to_string()],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        objective: "integration test objective".to_string(),
        work: WorkOutput {
            summary: "wrote scoped.py".to_string(),
        },
        attempt: 0,
        target_files: vec!["scoped.py".to_string()],
        validation_plan: Some(plan),
    });

    assert!(
        matches!(event, SchedulerEvent::IntegrationSucceeded { .. }),
        "target-scoped ValidationPlan must receive node targets; got: {event:#?}"
    );
}

// ── absent/preconditioned steps not executed ─────────────────────────────────

#[test]
fn preconditioned_step_skipped_when_file_absent() {
    // Invariant: a ValidationStep with when_artifacts_present is skipped — not
    // failed — when no workspace file matches any of its patterns.  Integration
    // must succeed even though the step command is `false`.
    let (_temp, artifact) = fixture("vplan-skip-absent");
    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    let guarded_failing_plan = ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["false".to_string()],
            when_artifacts_present: vec!["test_*.py".to_string()],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 30,
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        worker_role: None,
        kind: NodeKind::Work,
        objective: "write a file (no test files)".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        objective: "integration test objective".to_string(),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: Some(guarded_failing_plan),
    });

    assert!(
        matches!(event, SchedulerEvent::IntegrationSucceeded { .. }),
        "preconditioned step must be skipped when no matching file exists; got: {event:#?}"
    );
}
