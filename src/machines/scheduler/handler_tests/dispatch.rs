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
    let output = run_scheduler(handler(), state);
    assert!(
        matches!(output, SchedulerTerminalOutput::Complete { .. }),
        "expected Complete, got {output:#?}"
    );
}

#[test]
fn single_work_node_flows_through_runner_to_terminal_output() {
    // Invariant: StaticNodeRunner's success/failure rule (triggered by "fail"
    // in the objective) drives the scheduler to the matching terminal output
    // through the full RunNode → IntegrateWork loop.
    for (objective, expect_complete) in [("build artifacts", true), ("fail this step", false)] {
        let state = SchedulerState::Active {
            graph: RunGraph {
                nodes: vec![work_node("W", objective)],
                next_id: 0,
            },
            run_config: RunConfig::default(),
        };
        let output = run_scheduler(handler(), state);
        if expect_complete {
            assert!(
                matches!(output, SchedulerTerminalOutput::Complete { .. }),
                "[{objective}] expected Complete, got {output:#?}"
            );
        } else {
            assert!(
                matches!(output, SchedulerTerminalOutput::Failed { .. }),
                "[{objective}] expected Failed, got {output:#?}"
            );
        }
    }
}
