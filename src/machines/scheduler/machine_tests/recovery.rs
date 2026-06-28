use super::*;
use crate::validation::{ValidationPlan, ValidationScope, ValidationStage, ValidationStep};

#[test]
fn retry_creates_replacement_node() {
    let graph = RunGraph {
        nodes: vec![work_node("W", "do retry", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "first try failed".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "try again".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running")
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes.len(), 2);
    let replacement = &graph.nodes[1];
    assert_eq!(replacement.status, NodeStatus::Pending);
    assert_eq!(replacement.attempt, 1);
    assert_eq!(replacement.model_tier, ModelTier::Cheap);
    assert_eq!(replacement.objective, "do retry");
}

#[test]
fn validation_failure_creates_retry_feedback() {
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "fix main", &[])],
        next_id: 0,
    };
    graph.nodes[0].target_files = vec!["main.py".to_string()];
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("W".to_string()),
            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                kind: FailureKind::ValidationFailure,
                message: "validation failed".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "validation failed\ncommand: validate main.py\nexit code: 2\nfirst location: main.py:1:1\ndiagnostics:\nmain.py:1:1: invalid syntax\ninstruction: fix the existing file using file tools before accepting".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    let retry = &graph.nodes[1];
    assert_eq!(retry.status, NodeStatus::Pending);
    assert_eq!(retry.attempt, 1);
    assert_eq!(retry.target_files, vec!["main.py"]);
    assert!(retry.objective.starts_with("fix main"));
    assert!(retry.objective.contains("Target files: main.py"));
    assert!(retry.objective.contains("command: validate main.py"));
    assert!(retry.objective.contains("exit code: 2"));
    assert!(retry.objective.contains("first location: main.py:1:1"));
    assert!(retry.objective.contains("invalid syntax"));
    assert!(
        retry
            .objective
            .contains("fix the existing file using file tools before accepting")
    );
}

#[test]
fn work_semantic_validation_failure_retries_with_artifact_feedback() {
    let plan = ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["cargo".to_string(), "test".to_string()],
            when_artifacts_present: vec![],
            scope: ValidationScope::TargetFiles,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 60,
    };
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "modify src/lib.rs", &[])],
        next_id: 0,
    };
    graph.nodes[0].target_files = vec!["src/lib.rs".to_string(), "tests/lib.rs".to_string()];
    graph.nodes[0].validation_plan = Some(plan.clone());
    graph.nodes[0].status = NodeStatus::Running;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::WorkSemanticValidationFailure,
                message: "work semantic validation failed: accepted work did not produce an artifact update".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "Accepted Work results must modify the artifact. Use a file tool such as write_file, replace_text, or delete_file before returning accepted output.".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes.len(), 2, "scheduler must create a retry node");

    let retry = &graph.nodes[1];
    assert_eq!(retry.status, NodeStatus::Pending);
    assert_eq!(retry.kind, NodeKind::Work);
    assert_eq!(retry.attempt, 1);
    assert_eq!(
        retry.target_files,
        vec!["src/lib.rs".to_string(), "tests/lib.rs".to_string()]
    );
    assert_eq!(retry.validation_plan.as_ref(), Some(&plan));
    assert!(matches!(retry.origin, NodeOrigin::Retry { .. }));
    assert!(
        retry
            .objective
            .contains("Target files: src/lib.rs, tests/lib.rs"),
        "retry objective must preserve target files; got:\n{}",
        retry.objective
    );
    assert!(
        retry.objective.contains("must modify the artifact"),
        "retry objective must tell the Producer to modify the artifact; got:\n{}",
        retry.objective
    );
    assert!(
        retry.objective.contains("write_file"),
        "retry objective must tell the Producer to use a file tool; got:\n{}",
        retry.objective
    );
}

#[test]
fn retry_preserves_depth() {
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "do retry", &[])],
        next_id: 0,
    };
    graph.nodes[0].plan_depth = 7;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "first try failed".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "try again".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.nodes[1].plan_depth, 7);
}

#[test]
fn elevate_creates_replacement_node_with_strong_tier() {
    let graph = RunGraph {
        nodes: vec![work_node("W", "do elevate", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "needs stronger model".to_string(),
                recovery: RecoveryAction::ElevateModel {
                    message: "use strong".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running")
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes.len(), 2);
    let replacement = &graph.nodes[1];
    assert_eq!(replacement.status, NodeStatus::Pending);
    assert_eq!(replacement.attempt, 1);
    assert_eq!(replacement.model_tier, ModelTier::Strong);
    assert_eq!(replacement.objective, "do elevate");
}

#[test]
fn elevate_preserves_depth() {
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "do elevate", &[])],
        next_id: 0,
    };
    graph.nodes[0].plan_depth = 7;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "needs stronger model".to_string(),
                recovery: RecoveryAction::ElevateModel {
                    message: "use strong".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.nodes[1].plan_depth, 7);
}

#[test]
fn recovery_exhaustion_fails_scheduler() {
    // A node already at MAX_ATTEMPTS must not spawn a replacement regardless
    // of the recovery action; the scheduler transitions to Failed immediately.
    for (case, recovery) in [
        (
            "Retry",
            RecoveryAction::Retry {
                message: "try again".to_string(),
            },
        ),
        (
            "ElevateModel",
            RecoveryAction::ElevateModel {
                message: "escalate model".to_string(),
            },
        ),
        (
            "Split",
            RecoveryAction::Split {
                message: "decompose the work".to_string(),
            },
        ),
    ] {
        let mut node = work_node("W", "failing task", &[]);
        node.attempt = MAX_ATTEMPTS;
        let graph = RunGraph {
            nodes: vec![node],
            next_id: 0,
        };

        let t = do_transition(
            SchedulerState::Waiting {
                graph: running(graph, "W"),
                running: NodeId("W".to_string()),
            },
            SchedulerEvent::NodeReturned {
                node_id: NodeId("W".to_string()),
                outcome: NodeOutcome::Failed(NodeFailure {
                    kind: FailureKind::DeliberationFailure,
                    message: "transient error".to_string(),
                    recovery,
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("[{case}] expected Failed, got {:#?}", t.state);
        };
        assert_eq!(
            graph.nodes.len(),
            1,
            "[{case}] no replacement node should be created"
        );
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed, "[{case}]");
        assert!(
            reason.contains("exhausted"),
            "[{case}] reason should mention exhaustion, got: {reason:?}"
        );
        assert!(
            matches!(t.effects.as_slice(), [SchedulerEffect::ReturnFailed { .. }]),
            "[{case}]"
        );
    }
}

// ── Plan dependency validation tests ─────────────────────────────────────

#[test]
fn terminal_failure_cancels_downstream_chain() {
    // Graph: A -> B -> C -> D
    // A is already Completed, B is Running and fails terminally.
    // Expected final statuses: A=Completed, B=Failed, C=Cancelled, D=Cancelled.
    let mut graph = RunGraph {
        nodes: vec![
            work_node("A", "step A", &[]),
            work_node("B", "step B", &["A"]),
            work_node("C", "step C", &["B"]),
            work_node("D", "step D", &["C"]),
        ],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Completed;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "B"),
            running: NodeId("B".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "unrecoverable".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "fatal error".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Failed { graph, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };

    let status = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|n| n.id.0 == id)
            .unwrap_or_else(|| panic!("node {id} not found"))
            .status
            .clone()
    };

    assert_eq!(status("A"), NodeStatus::Completed);
    assert_eq!(status("B"), NodeStatus::Failed);
    assert_eq!(status("C"), NodeStatus::Cancelled);
    assert_eq!(status("D"), NodeStatus::Cancelled);
}

#[test]
fn split_below_attempt_limit_still_creates_plan_node() {
    // A node at attempt 0 (below MAX_ATTEMPTS) must still produce a Split
    // Plan node with attempt incremented to 1, and must remap downstream deps.
    let mut graph = RunGraph {
        nodes: vec![
            work_node("A", "step A", &[]),
            work_node("W", "complex task", &["A"]),
            work_node("C", "step C", &["W"]),
        ],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Completed;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "task too complex".to_string(),
                recovery: RecoveryAction::Split {
                    message: "decompose the work".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };

    // Original W is Failed.
    let w = graph.nodes.iter().find(|n| n.id.0 == "W").expect("W");
    assert_eq!(w.status, NodeStatus::Failed);

    // Split Plan node exists with attempt=1 and Strong tier.
    let split = graph
        .nodes
        .iter()
        .find(|n| n.id.0.starts_with("W-split-"))
        .expect("split Plan node");
    assert_eq!(split.kind, NodeKind::Plan);
    assert_eq!(split.status, NodeStatus::Pending);
    assert_eq!(split.attempt, 1, "split Plan node must carry attempt + 1");
    assert_eq!(split.model_tier, ModelTier::Strong);

    // C's dependency was rewritten from W to the split Plan node.
    let c = graph.nodes.iter().find(|n| n.id.0 == "C").expect("C");
    assert!(
        !c.dependencies.contains(&NodeId("W".to_string())),
        "C must not depend on failed W"
    );
    assert!(
        c.dependencies.contains(&split.id),
        "C must depend on the split Plan node"
    );
}

#[test]
fn integration_failure_terminal_cancels_downstream_dependents() {
    let mut graph = RunGraph {
        nodes: vec![
            work_node("A", "step A", &[]),
            work_node("B", "step B", &["A"]),
            work_node("C", "step C", &["B"]),
            work_node("D", "step D", &["C"]),
        ],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Completed;
    graph.nodes[1].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("B".to_string()),
        },
        SchedulerEvent::IntegrationReturned {
            node_id: NodeId("B".to_string()),
            outcome: IntegrationOutcome::Failed(IntegrationFailure {
                kind: FailureKind::IntegrationFailure,
                message: "integration error".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "integration cannot be recovered".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Failed { graph, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };

    let status = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|n| n.id.0 == id)
            .unwrap_or_else(|| panic!("node {id} not found"))
            .status
            .clone()
    };

    assert_eq!(status("B"), NodeStatus::Failed);
    assert_eq!(status("C"), NodeStatus::Cancelled);
    assert_eq!(status("D"), NodeStatus::Cancelled);
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn integration_failure_exhaustion_fails_scheduler() {
    // A node in Integrating status at MAX_ATTEMPTS must not spawn a replacement
    // regardless of the recovery action; the scheduler transitions to Failed.
    for (case, recovery) in [
        (
            "Retry",
            RecoveryAction::Retry {
                message: "retry integration".to_string(),
            },
        ),
        (
            "ElevateModel",
            RecoveryAction::ElevateModel {
                message: "use stronger model".to_string(),
            },
        ),
        (
            "Split",
            RecoveryAction::Split {
                message: "decompose step B".to_string(),
            },
        ),
    ] {
        let mut node = work_node("B", "step B", &[]);
        node.status = NodeStatus::Integrating;
        node.attempt = MAX_ATTEMPTS;
        let graph = RunGraph {
            nodes: vec![node],
            next_id: 0,
        };

        let t = do_transition(
            SchedulerState::Waiting {
                graph,
                running: NodeId("B".to_string()),
            },
            SchedulerEvent::IntegrationReturned {
                node_id: NodeId("B".to_string()),
                outcome: IntegrationOutcome::Failed(IntegrationFailure {
                    kind: FailureKind::IntegrationFailure,
                    message: "integration error".to_string(),
                    recovery,
                }),
            },
        );

        let SchedulerState::Failed { graph, reason } = t.state else {
            panic!("[{case}] expected Failed, got {:#?}", t.state);
        };
        assert_eq!(
            graph.nodes.len(),
            1,
            "[{case}] no replacement should be created"
        );
        assert_eq!(graph.nodes[0].status, NodeStatus::Failed, "[{case}]");
        assert!(
            reason.contains("exhausted"),
            "[{case}] reason should mention exhaustion, got: {reason:?}"
        );
        assert!(
            matches!(t.effects.as_slice(), [SchedulerEffect::ReturnFailed { .. }]),
            "[{case}]"
        );
    }
}

// ── Deadlock diagnostics tests ────────────────────────────────────────────

#[test]
fn single_tier_elevate_falls_back_to_retry() {
    // has_strong_tier: false → ElevateModel must not create a Strong replacement;
    // it must fall back to Retry, preserving the original model tier.
    let graph = RunGraph {
        nodes: vec![work_node("W", "do elevate", &[])],
        next_id: 0,
    };
    let t = SchedulerMachine {
        has_strong_tier: false,
    }
    .transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "needs stronger model".to_string(),
                recovery: RecoveryAction::ElevateModel {
                    message: "use strong".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 2, "must create a replacement node");
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    let replacement = &graph.nodes[1];
    assert!(
        matches!(replacement.origin, NodeOrigin::Retry { .. }),
        "single-tier ElevateModel must fall back to Retry, got origin: {:?}",
        replacement.origin
    );
    assert_eq!(
        replacement.model_tier,
        ModelTier::Cheap,
        "fallback Retry must preserve the original Cheap tier"
    );
    assert!(t.effects.is_empty());
}

#[test]
fn multi_tier_elevate_creates_strong_replacement() {
    // has_strong_tier: true → ElevateModel on a Cheap-tier node must produce a
    // Strong-tier replacement.
    let graph = RunGraph {
        nodes: vec![work_node("W", "do elevate", &[])],
        next_id: 0,
    };
    let t = SchedulerMachine {
        has_strong_tier: true,
    }
    .transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "needs stronger model".to_string(),
                recovery: RecoveryAction::ElevateModel {
                    message: "use strong".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert_eq!(graph.nodes.len(), 2);
    let replacement = &graph.nodes[1];
    assert_eq!(replacement.model_tier, ModelTier::Strong);
    assert!(
        matches!(replacement.origin, NodeOrigin::ElevateModel { .. }),
        "multi-tier must produce ElevateModel replacement"
    );
}

#[test]
fn single_tier_elevate_exhausted_gives_clear_terminal_failure() {
    // has_strong_tier: false + MAX_ATTEMPTS → Terminal with "no higher model tier available"
    // in the reason string.
    let mut node = work_node("W", "hard task", &[]);
    node.attempt = MAX_ATTEMPTS;
    let graph = RunGraph {
        nodes: vec![node],
        next_id: 0,
    };
    let t = SchedulerMachine {
        has_strong_tier: false,
    }
    .transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "capability ceiling".to_string(),
                recovery: RecoveryAction::ElevateModel {
                    message: "escalate model".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Failed { graph, reason } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 1, "no replacement should be created");
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    assert!(
        reason.contains("no higher model tier available"),
        "reason must mention no higher model tier, got: {reason:?}"
    );
    assert!(
        reason.contains("exhausted") || reason.contains(&MAX_ATTEMPTS.to_string()),
        "reason must mention attempt exhaustion, got: {reason:?}"
    );
    assert!(matches!(
        t.effects.as_slice(),
        [SchedulerEffect::ReturnFailed { .. }]
    ));
}

#[test]
fn elevate_at_strong_tier_falls_back_to_retry() {
    // A node already running at ModelTier::Strong has no higher tier to go to
    // even with has_strong_tier: true. Must fall back to Retry.
    let mut node = work_node("W", "hard task at strong", &[]);
    node.model_tier = ModelTier::Strong;
    let graph = RunGraph {
        nodes: vec![node],
        next_id: 0,
    };
    let t = SchedulerMachine {
        has_strong_tier: true,
    }
    .transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            running: NodeId("W".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("W".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "still failing at strong tier".to_string(),
                recovery: RecoveryAction::ElevateModel {
                    message: "use even stronger".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    assert_eq!(graph.nodes.len(), 2, "must create a Retry replacement");
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
    let replacement = &graph.nodes[1];
    assert!(
        matches!(replacement.origin, NodeOrigin::Retry { .. }),
        "Strong-tier node with ElevateModel must fall back to Retry"
    );
}

#[test]
fn terminal_failure_does_not_touch_completed_nodes() {
    // Graph: A -> B -> C
    // A is Completed, B is Running and fails terminally.
    // A must remain Completed; only C (Pending) should be Cancelled.
    let mut graph = RunGraph {
        nodes: vec![
            work_node("A", "step A", &[]),
            work_node("B", "step B", &["A"]),
            work_node("C", "step C", &["B"]),
        ],
        next_id: 0,
    };
    graph.nodes[0].status = NodeStatus::Completed;

    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "B"),
            running: NodeId("B".to_string()),
        },
        SchedulerEvent::NodeReturned {
            node_id: NodeId("B".to_string()),
            outcome: NodeOutcome::Failed(NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "unrecoverable".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "fatal error".to_string(),
                },
            }),
        },
    );

    let SchedulerState::Failed { graph, .. } = t.state else {
        panic!("expected Failed, got {:#?}", t.state);
    };

    let a = graph.nodes.iter().find(|n| n.id.0 == "A").unwrap();
    let b = graph.nodes.iter().find(|n| n.id.0 == "B").unwrap();
    let c = graph.nodes.iter().find(|n| n.id.0 == "C").unwrap();

    assert_eq!(a.status, NodeStatus::Completed, "A must remain Completed");
    assert_eq!(b.status, NodeStatus::Failed);
    assert_eq!(c.status, NodeStatus::Cancelled);
}

fn validation_retry_event(node_id: &str, diagnostics: &str) -> SchedulerEvent {
    SchedulerEvent::IntegrationReturned {
        node_id: NodeId(node_id.to_string()),
        outcome: IntegrationOutcome::Failed(IntegrationFailure {
            kind: FailureKind::ValidationFailure,
            message: "validation failed".to_string(),
            recovery: RecoveryAction::Retry {
                message: diagnostics.to_string(),
            },
        }),
    }
}

#[test]
fn validation_retry_feedback_includes_all_structured_target_files() {
    // Invariant: structured target_files drives the "Target files:" line; every
    // file present in target_files must appear in the retry objective.
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "fix main", &[])],
        next_id: 0,
    };
    graph.nodes[0].target_files = vec!["main.py".to_string(), "test_main.py".to_string()];
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("W".to_string()),
        },
        validation_retry_event(
            "W",
            "validation failed\ncommand: pytest\nexit code: 1\nfirst location: (not detected)\ndiagnostics:\n0 tests ran\ninstruction: fix the existing file using file tools before accepting",
        ),
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    let retry = &graph.nodes[1];
    assert!(
        retry.objective.contains("main.py"),
        "main.py must appear in retry objective"
    );
    assert!(
        retry.objective.contains("test_main.py"),
        "test_main.py must appear in retry objective"
    );
    assert!(
        retry
            .objective
            .contains("Target files: main.py, test_main.py"),
        "both files must appear together in Target files line; got:\n{}",
        retry.objective
    );
}

#[test]
fn validation_retry_test_target_appears_in_retry_prompt() {
    // Invariant: a test-file target added to structured target_files is visible
    // to the retry node and is not silently dropped from the feedback.
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "write and test a feature", &[])],
        next_id: 0,
    };
    graph.nodes[0].target_files = vec!["src/lib.rs".to_string(), "tests/lib_test.rs".to_string()];
    graph.nodes[0].status = NodeStatus::Integrating;

    let t = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("W".to_string()),
        },
        validation_retry_event(
            "W",
            "validation failed\ncommand: cargo test\nexit code: 101\nfirst location: (not detected)\ndiagnostics:\ntest failed\ninstruction: fix the existing file using file tools before accepting",
        ),
    );

    let SchedulerState::Running { graph } = t.state else {
        panic!("expected Running, got {:#?}", t.state);
    };
    let retry = &graph.nodes[1];
    assert!(
        retry.objective.contains("tests/lib_test.rs"),
        "test target must appear in retry objective; got:\n{}",
        retry.objective
    );
}

#[test]
fn repeated_validation_retries_do_not_duplicate_feedback_blocks() {
    // Invariant: each retry replaces the previous feedback block with the latest
    // diagnostics; old blocks must not accumulate across multiple retries.
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "fix main", &[])],
        next_id: 0,
    };
    graph.nodes[0].target_files = vec!["main.py".to_string()];
    graph.nodes[0].status = NodeStatus::Integrating;

    let t1 = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("W".to_string()),
        },
        validation_retry_event(
            "W",
            "validation failed\ncommand: val\nexit code: 1\nfirst location: main.py:1:1\ndiagnostics:\nfirst-error\ninstruction: fix the existing file using file tools before accepting",
        ),
    );
    let SchedulerState::Running { mut graph } = t1.state else {
        panic!("expected Running after first retry");
    };

    let retry1_id = graph.nodes[1].id.clone();
    graph.nodes[1].status = NodeStatus::Integrating;

    let t2 = do_transition(
        SchedulerState::Waiting {
            graph,
            running: retry1_id.clone(),
        },
        validation_retry_event(
            &retry1_id.0,
            "validation failed\ncommand: val\nexit code: 2\nfirst location: main.py:2:1\ndiagnostics:\nsecond-error\ninstruction: fix the existing file using file tools before accepting",
        ),
    );
    let SchedulerState::Running { graph } = t2.state else {
        panic!("expected Running after second retry");
    };
    let retry2 = &graph.nodes[2];

    let block_count = retry2
        .objective
        .matches("Validation feedback for retry:")
        .count();
    assert_eq!(
        block_count, 1,
        "feedback block must appear exactly once after two retries"
    );
    assert!(
        retry2.objective.starts_with("fix main"),
        "clean original objective must lead"
    );
    assert!(
        retry2.objective.contains("second-error"),
        "latest diagnostics must be present"
    );
    assert!(
        !retry2.objective.contains("first-error"),
        "stale diagnostics must not accumulate"
    );
}

#[test]
fn retry_target_files_unchanged_across_retries() {
    // Invariant: structured target_files on the retry node is always identical
    // to the original node's target_files, regardless of retry count.
    let mut graph = RunGraph {
        nodes: vec![work_node("W", "fix main", &[])],
        next_id: 0,
    };
    let original_targets = vec!["main.py".to_string(), "test_main.py".to_string()];
    graph.nodes[0].target_files = original_targets.clone();
    graph.nodes[0].status = NodeStatus::Integrating;

    let t1 = do_transition(
        SchedulerState::Waiting {
            graph,
            running: NodeId("W".to_string()),
        },
        validation_retry_event(
            "W",
            "validation failed\ncommand: v\nexit code: 1\nfirst location: (not detected)\ndiagnostics:\n(no diagnostic output)\ninstruction: fix the existing file using file tools before accepting",
        ),
    );
    let SchedulerState::Running { mut graph } = t1.state else {
        panic!("expected Running after first retry");
    };
    assert_eq!(
        graph.nodes[1].target_files, original_targets,
        "target_files after retry 1"
    );

    let retry1_id = graph.nodes[1].id.clone();
    graph.nodes[1].status = NodeStatus::Integrating;

    let t2 = do_transition(
        SchedulerState::Waiting {
            graph,
            running: retry1_id.clone(),
        },
        validation_retry_event(
            &retry1_id.0,
            "validation failed\ncommand: v\nexit code: 2\nfirst location: (not detected)\ndiagnostics:\n(no diagnostic output)\ninstruction: fix the existing file using file tools before accepting",
        ),
    );
    let SchedulerState::Running { graph } = t2.state else {
        panic!("expected Running after second retry");
    };
    assert_eq!(
        graph.nodes[2].target_files, original_targets,
        "target_files after retry 2"
    );
}
