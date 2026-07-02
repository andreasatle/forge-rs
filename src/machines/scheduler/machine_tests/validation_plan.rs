use super::*;
use crate::validation::{ValidationPlan, ValidationScope, ValidationStage, ValidationStep};

fn one_step_plan() -> ValidationPlan {
    ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["true".to_string()],
            when_artifacts_present: vec![],
            scope: ValidationScope::TargetFiles,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 30,
    }
}

fn node_with_plan(id: &str, objective: &str, plan: ValidationPlan) -> Node {
    let mut n = work_node(id, objective, &[]);
    n.validation_plan = Some(plan);
    n
}

// ── node stores plan ─────────────────────────────────────────────────────────

#[test]
fn plan_expansion_stamps_validation_plan_onto_work_children() {
    // Invariant: when PlanAccepted carries NodeRequests with a validation_plan,
    // insert_children must copy that plan onto the inserted graph nodes.
    let plan = one_step_plan();
    let graph = RunGraph {
        nodes: vec![plan_node("P", "plan it", &[])],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "P"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::PlanAccepted {
            node_id: NodeId("P".to_string()),
            plan: PlanOutput {
                children: vec![NodeRequest {
                    id: NodeId("work".to_string()),
                    kind: NodeKind::Work,
                    objective: "do the work".to_string(),
                    target_files: vec![],
                    required_validation_targets: vec![],
                    dependencies: vec![],
                    validation_plan: Some(plan.clone()),
                }],
            },
        },
    );

    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active");
    };
    let work = graph
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Work)
        .expect("work node must be present");
    assert_eq!(
        work.validation_plan.as_ref(),
        Some(&plan),
        "inserted work node must carry the validation_plan from the NodeRequest"
    );
}

// ── checkpoint roundtrip preserves plan ──────────────────────────────────────

#[test]
fn checkpoint_roundtrip_preserves_validation_plan_in_node() {
    // Invariant: a SchedulerState serialized to JSON and restored is byte-equal
    // to the original, including each node's validation_plan.
    use crate::runtime::checkpoint::{load_checkpoint, save_checkpoint};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("forge-vplan-ckpt-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let plan = one_step_plan();
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![node_with_plan("W", "work with plan", plan.clone())],
            next_id: 0,
        },
        run_config: RunConfig::default(),
    };

    save_checkpoint(&dir, &state).unwrap();
    let loaded = load_checkpoint(&dir).unwrap();

    let SchedulerState::Active { graph, .. } = loaded else {
        panic!("expected Active");
    };
    assert_eq!(
        graph.nodes[0].validation_plan.as_ref(),
        Some(&plan),
        "checkpoint roundtrip must preserve the node's validation_plan exactly"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── retry preserves plan ─────────────────────────────────────────────────────

#[test]
fn retry_preserves_validation_plan() {
    // Invariant: when a Work node with a validation_plan is retried, the
    // replacement node carries the same plan.
    let plan = one_step_plan();
    let graph = RunGraph {
        nodes: vec![node_with_plan("W", "do work", plan.clone())],
        next_id: 0,
    };
    let t = do_transition(
        SchedulerState::Waiting {
            graph: running(graph, "W"),
            run_config: RunConfig::default(),
        },
        SchedulerEvent::NodeFailed {
            node_id: NodeId("W".to_string()),
            failure: NodeFailure {
                kind: FailureKind::DeliberationFailure,
                message: "first try failed".to_string(),
                recovery: RecoveryAction::Retry {
                    message: "try again".to_string(),
                },
            },
        },
    );

    let SchedulerState::Active { graph, .. } = t.state else {
        panic!("expected Active");
    };
    let retry = graph
        .nodes
        .iter()
        .find(|n| n.status == NodeStatus::Pending)
        .expect("retry node must be present");
    assert_eq!(
        retry.validation_plan.as_ref(),
        Some(&plan),
        "retry node must carry the same validation_plan as the original node"
    );
}

// ── checkpoint does not change plan after config change ──────────────────────

#[test]
fn checkpointed_plan_is_independent_of_later_config() {
    // Invariant: a node's validation_plan is captured at creation time.
    // Loading a checkpoint always restores the original plan, regardless of
    // what any caller would pass if constructing the runner today.
    use crate::runtime::checkpoint::{load_checkpoint, save_checkpoint};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("forge-vplan-config-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let original_plan = one_step_plan();
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![node_with_plan("W", "work", original_plan.clone())],
            next_id: 0,
        },
        run_config: RunConfig::default(),
    };

    save_checkpoint(&dir, &state).unwrap();

    // Simulate "config changed": caller would now provide a different plan or
    // no plan at all.  The checkpoint must reflect the original plan only.
    let later_plan = ValidationPlan {
        steps: vec![ValidationStep {
            command: vec!["false".to_string()],
            when_artifacts_present: vec![],
            scope: ValidationScope::Workspace,
            stage: ValidationStage::PreIntegration,
            must_pass: true,
        }],
        timeout_seconds: 5,
    };

    let loaded = load_checkpoint(&dir).unwrap();
    let SchedulerState::Active { graph, .. } = loaded else {
        panic!("expected Active");
    };
    assert_ne!(
        graph.nodes[0].validation_plan.as_ref(),
        Some(&later_plan),
        "checkpointed plan must not match a different plan constructed after the fact"
    );
    assert_eq!(
        graph.nodes[0].validation_plan.as_ref(),
        Some(&original_plan),
        "checkpointed plan must match the plan set at node creation time"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
