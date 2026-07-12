use super::*;

// ── workspace cleanup tests ───────────────────────────────────────────────

/// Captures the workspace path and controls whether validation passes or fails.
struct PathCapturingValidator {
    captured: Rc<RefCell<Option<std::path::PathBuf>>>,
    pass: bool,
}

impl Validator for PathCapturingValidator {
    fn validate(&self, workspace: &Workspace) -> ValidationResult {
        *self.captured.borrow_mut() = Some(workspace.path().to_path_buf());
        ValidationResult {
            passed: self.pass,
            summary: "path-capturing validator".to_string(),
            failure: None,
        }
    }
}

#[test]
fn temporary_workspace_removed_after_integration() {
    // Invariant: the WorkAttempt's temporary workspace is always removed
    // after IntegrateWork, regardless of whether the validator passes or
    // fails the attempt.
    for pass in [true, false] {
        let (_temp, artifact) = fixture(&format!("temp-removed-pass-{pass}"));
        let runner = FileWritingRunner {
            path: "output.txt".to_string(),
            content: "hello\n".to_string(),
        };
        let captured = Rc::new(RefCell::new(None));
        let validator = PathCapturingValidator {
            captured: captured.clone(),
            pass,
        };
        let h =
            SchedulerHandler::with_artifact(runner, artifact).with_validator(Rc::new(validator));

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

        h.handle_effect(SchedulerEffect::IntegrateWork {
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

        let path = captured
            .borrow()
            .clone()
            .expect("validator must have been called");
        assert!(
            !path.exists(),
            "[pass={pass}] temporary workspace must be removed after integration"
        );
    }
}

// ── checkpoint tests ──────────────────────────────────────────────────────

#[test]
fn checkpoint_written_after_node_returned() {
    use crate::machines::scheduler::run_scheduler;
    use crate::runtime::checkpoint::load_checkpoint;

    let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("forge-handler-ckpt-{}-{seq}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "do some work")],
        },
        run_config: RunConfig::default(),
    };
    run_scheduler(
        SchedulerHandler::new(StaticNodeRunner).with_checkpoint_dir(dir.clone()),
        state,
    );

    let checkpoint_path = dir.join("graph.json");
    assert!(
        checkpoint_path.exists(),
        "graph.json must be written after run"
    );
    // The checkpoint captures the last non-terminal state (Active, not Complete).
    // The final Complete state is a terminal and is never checkpointed.
    let loaded = load_checkpoint(&dir).unwrap();
    let SchedulerState::Active { graph, .. } = loaded else {
        panic!("expected Active state in checkpoint");
    };
    assert!(
        graph
            .nodes
            .iter()
            .all(|n| n.status == NodeStatus::Completed),
        "all nodes must be Completed in the final checkpoint"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_load_round_trip() {
    use crate::runtime::checkpoint::{load_checkpoint, save_checkpoint};

    let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "forge-handler-ckpt-rt-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![
                Node {
                    id: NodeId("A".to_string()),
                    kind: NodeKind::Work,
                    team: String::new(),
                    task_id: None,
                    adapter: String::new(),
                    northstar: String::new(),
                    worker_role: None,
                    objective: "do A".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![],
                    status: NodeStatus::Completed,
                    attempt: 0,
                    plan_depth: 0,
                    model_tier: ModelTier::Cheap,
                    summary: Some("done".to_string()),
                    origin: NodeOrigin::Root,
                    validation_plan: None,
                    retry_feedback: None,
                },
                work_node("B", "do B"),
            ],
        },
        run_config: RunConfig::default(),
    };

    save_checkpoint(&dir, &state).unwrap();
    let loaded = load_checkpoint(&dir).unwrap();
    assert_eq!(state, loaded);

    let _ = fs::remove_dir_all(&dir);
}
