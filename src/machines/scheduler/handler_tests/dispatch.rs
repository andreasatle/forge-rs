use super::*;

// ── existing tests (unchanged) ────────────────────────────────────────────

#[test]
fn run_node_effect_uses_node_runner() {
    let h = handler();
    let effect = SchedulerEffect::RunNode {
        node_id: NodeId("n1".to_string()),
        kind: NodeKind::Work,
        objective: "write some code".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    };
    let event = h.handle_effect(effect);
    let SchedulerEvent::WorkAccepted { .. } = event else {
        panic!("expected WorkAccepted, got {event:#?}");
    };
}

#[test]
fn plan_node_flows_through_runner() {
    let state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "plan the work".to_string(),
        },
        RunConfig::default(),
    );
    let output = run_machine(handler(), state);
    assert!(
        matches!(output, SchedulerOutput::Complete { .. }),
        "expected Complete, got {output:#?}"
    );
}

#[test]
fn work_node_flows_through_runner() {
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "build artifacts")],
            next_id: 0,
        },
        run_config: RunConfig::default(),
    };
    let output = run_machine(handler(), state);
    assert!(
        matches!(output, SchedulerOutput::Complete { .. }),
        "expected Complete, got {output:#?}"
    );
}

#[test]
fn failed_node_flows_through_runner() {
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("F", "fail this step")],
            next_id: 0,
        },
        run_config: RunConfig::default(),
    };
    let output = run_machine(handler(), state);
    assert!(
        matches!(output, SchedulerOutput::Failed { .. }),
        "expected Failed, got {output:#?}"
    );
}
