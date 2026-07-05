use super::*;

// ── telemetry tests ───────────────────────────────────────────────────────

/// A node runner that always fails with a fixed reason.
struct AlwaysFailRunner {
    reason: String,
}

impl NodeRunner for AlwaysFailRunner {
    fn run_node(&self, _request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        use crate::machines::scheduler::{NodeFailure, RecoveryAction};
        NodeRunResult::Failed(NodeFailure {
            kind: FailureKind::DeliberationFailure,
            message: self.reason.clone(),
            recovery: RecoveryAction::Terminal {
                message: "terminal".to_string(),
            },
        })
    }
}

#[test]
fn node_failure_reason_preserved_in_full_in_telemetry() {
    use crate::machines::scheduler::run_scheduler_with_telemetry;
    use crate::telemetry::FileTelemetry;

    let long_reason = "provider error: connection timed out after 3 retries; \
            last attempt returned status 503; node objective was 'write the implementation'; \
            this reason must appear verbatim in the telemetry file and must not be elided to '...'";

    let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "forge-handler-telemetry-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    let sink = FileTelemetry::new(dir.clone());

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("fail-node", "do some work")],
        },
        run_config: RunConfig::default(),
    };

    run_scheduler_with_telemetry(
        SchedulerHandler::new(AlwaysFailRunner {
            reason: long_reason.to_string(),
        }),
        state,
        &sink,
    );

    let all_content: String = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");

    let _ = fs::remove_dir_all(&dir);

    assert!(
        all_content.contains(long_reason),
        "telemetry must contain the full failure reason; got:\n{all_content}"
    );
    assert!(
        !all_content.contains("reason: \"...\""),
        "telemetry must not elide the failure reason to '...'; got:\n{all_content}"
    );
}

#[test]
fn telemetry_failure_does_not_change_scheduler_behavior() {
    use crate::machines::scheduler::run_scheduler_with_telemetry;
    use crate::telemetry::FileTelemetry;

    let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "forge-handler-tel-fail-sched-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    // Create the sink, then delete the directory so all writes fail.
    let sink = FileTelemetry::new(dir.clone());
    let _ = fs::remove_dir_all(&dir);
    let shared: Rc<dyn TelemetrySink> = Rc::new(sink);

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "do some work")],
        },
        run_config: RunConfig::default(),
    };
    let output = run_scheduler_with_telemetry(
        SchedulerHandler::new(StaticNodeRunner).with_telemetry(Rc::clone(&shared)),
        state,
        shared.as_ref(),
    );
    assert!(
        matches!(output.0, SchedulerTerminalOutput::Complete { .. }),
        "scheduler output must be Complete regardless of telemetry failures; got: {:#?}",
        output.0
    );
}

#[test]
fn artifact_commit_still_succeeds_when_telemetry_fails() {
    use crate::machines::scheduler::run_scheduler_with_telemetry;
    use crate::telemetry::FileTelemetry;

    let (_temp, artifact) = fixture("tel-fail-commit");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let seq = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "forge-handler-tel-fail-commit-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    // Create the sink, then delete the directory so all writes fail.
    let sink = FileTelemetry::new(dir.clone());
    let _ = fs::remove_dir_all(&dir);
    let shared: Rc<dyn TelemetrySink> = Rc::new(sink);

    let runner = FileWritingRunner {
        path: "result.txt".to_string(),
        content: "committed despite telemetry failure\n".to_string(),
    };
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W", "write a file")],
        },
        run_config: RunConfig::default(),
    };
    let (output, handler) = run_scheduler_with_telemetry(
        SchedulerHandler::with_artifact(runner, artifact).with_telemetry(Rc::clone(&shared)),
        state,
        shared.as_ref(),
    );

    assert!(
        matches!(output, SchedulerTerminalOutput::Complete { .. }),
        "run must complete even when telemetry writes all fail; got: {output:#?}"
    );

    let new_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_ne!(
        new_sha, original_sha,
        "artifact commit must advance even when telemetry fails"
    );

    let final_artifact = handler.artifact().expect("artifact must be present");
    assert_eq!(
        final_artifact.commit_sha, new_sha,
        "handler artifact must reflect the committed SHA"
    );
}

// ── shared-trace tests ────────────────────────────────────────────────────

/// Scripted provider for shared-trace tests.
struct ScriptedProvider {
    responses: RefCell<std::collections::VecDeque<String>>,
}

impl ScriptedProvider {
    fn from_strs(responses: &[&str]) -> Self {
        Self {
            responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
        }
    }
}

impl crate::providers::ProviderClient for ScriptedProvider {
    fn call(
        &self,
        _req: crate::providers::ProviderRequest,
    ) -> Result<crate::providers::ProviderResponse, crate::providers::ProviderError> {
        let content = self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("ScriptedProvider: responses exhausted");
        Ok(crate::providers::ProviderResponse {
            content,
            finish_reason: None,
        })
    }
}

#[test]
fn scheduler_and_deliberation_share_one_trace() {
    use crate::machines::scheduler::run_scheduler_with_telemetry;
    use crate::node_runner::DeliberatingNodeRunner;
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let vec_tel = Rc::new(VecTelemetry::new());
    let shared: Rc<dyn TelemetrySink> = vec_tel.clone();

    // Root Decomposition node (atomic, escalates to one Plan child) + Plan
    // node + work node, each requiring 3 provider calls (producer, critic,
    // referee).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"kind":"plan","tasks":[]}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
        r#"{"tasks":[{"id":"implement","objective":"implement it","operation":"create","targets":["output.txt"],"depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
        r#"{"summary":"work completed"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "do something".to_string(),
        },
        RunConfig::default(),
    );

    let handler = SchedulerHandler::new(runner).with_telemetry(Rc::clone(&shared));
    let _ = run_scheduler_with_telemetry(handler, initial_state, shared.as_ref());

    let records = vec_tel.records();
    let machine_names: Vec<&str> = records
        .iter()
        .filter_map(|record| match &record.event {
            TelemetryEvent::MachineStarted { machine } => Some(machine.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        machine_names.contains(&"SchedulerMachine"),
        "expected SchedulerMachine in shared trace; got: {machine_names:?}"
    );
    assert!(
        machine_names.contains(&"DeliberationMachine"),
        "expected DeliberationMachine in shared trace; got: {machine_names:?}"
    );
}

#[test]
fn nested_machine_events_preserve_order() {
    use crate::machines::scheduler::run_scheduler_with_telemetry;
    use crate::node_runner::DeliberatingNodeRunner;
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let vec_tel = Rc::new(VecTelemetry::new());
    let shared: Rc<dyn TelemetrySink> = vec_tel.clone();

    // Root Decomposition node (atomic, escalates to one Plan child) + Plan
    // node + work node, each requiring 3 provider calls (producer, critic,
    // referee).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"kind":"plan","tasks":[]}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
        r#"{"tasks":[{"id":"implement","objective":"implement it","operation":"create","targets":["output.txt"],"depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
        r#"{"summary":"work completed"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "do something".to_string(),
        },
        RunConfig::default(),
    );

    let handler = SchedulerHandler::new(runner).with_telemetry(Rc::clone(&shared));
    let _ = run_scheduler_with_telemetry(handler, initial_state, shared.as_ref());

    let records = vec_tel.records();
    let machine_seq: Vec<&str> = records
        .iter()
        .filter_map(|record| match &record.event {
            TelemetryEvent::MachineStarted { machine } => Some(machine.as_str()),
            _ => None,
        })
        .collect();

    let sched_pos = machine_seq
        .iter()
        .position(|&m| m == "SchedulerMachine")
        .expect("SchedulerMachine must appear in trace");
    let delib_pos = machine_seq
        .iter()
        .position(|&m| m == "DeliberationMachine")
        .expect("DeliberationMachine must appear in trace");

    assert!(
        sched_pos < delib_pos,
        "SchedulerMachine must start before DeliberationMachine; positions: {sched_pos} vs {delib_pos}"
    );

    // Verify scheduler events appear after deliberation finishes (EffectEmitted
    // is recorded before handle_effect; StateEntered of the next scheduler loop
    // iteration appears after the deliberation run completes).
    let last_delib_idx = records
        .iter()
        .rposition(|record| match &record.event {
            TelemetryEvent::StateEntered { machine, .. }
            | TelemetryEvent::EventReceived { machine, .. }
            | TelemetryEvent::EffectEmitted { machine, .. } => machine == "DeliberationMachine",
            _ => false,
        })
        .expect("deliberation must emit at least one event");

    let sched_after = records
        .iter()
        .skip(last_delib_idx + 1)
        .any(|record| match &record.event {
            TelemetryEvent::StateEntered { machine, .. }
            | TelemetryEvent::EventReceived { machine, .. } => machine == "SchedulerMachine",
            _ => false,
        });

    assert!(
        sched_after,
        "SchedulerMachine must have events after DeliberationMachine finishes"
    );
}

/// SchedulerMachine's `EffectEmitted` records for `RunNode` must carry the
/// dispatched node's id and attempt, so a viewer can tell which node a
/// scheduler-level effect belongs to without decoding the pretty-printed
/// effect body. `StateEntered`/`EventReceived` describe the whole run graph,
/// not a single node, so they must stay unstamped.
#[test]
fn scheduler_effect_emitted_carries_node_context_for_run_node() {
    use crate::machines::scheduler::run_scheduler_with_telemetry;
    use crate::node_runner::DeliberatingNodeRunner;
    use crate::telemetry::{TelemetryEvent, VecTelemetry};

    let vec_tel = Rc::new(VecTelemetry::new());
    let shared: Rc<dyn TelemetrySink> = vec_tel.clone();

    // Root Decomposition node (atomic, escalates to one Plan child) + Plan
    // node + work node, each requiring 3 provider calls (producer, critic,
    // referee).
    let provider = ScriptedProvider::from_strs(&[
        r#"{"kind":"plan","tasks":[]}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
        r#"{"tasks":[{"id":"implement","objective":"implement it","operation":"create","targets":["output.txt"],"depends_on":[]}]}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
        r#"{"summary":"work completed"}"#,
        r#"{"status":"accepted","content":"looks good"}"#,
        r#"{"status":"accepted","content":"approved"}"#,
    ]);
    let runner = DeliberatingNodeRunner::new(&provider, &provider);
    let initial_state = SchedulerMachine::initial_state(
        RunRequest {
            objective: "do something".to_string(),
        },
        RunConfig::default(),
    );

    let handler = SchedulerHandler::new(runner).with_telemetry(Rc::clone(&shared));
    let _ = run_scheduler_with_telemetry(handler, initial_state, shared.as_ref());

    let records = vec_tel.records();
    let run_node_effects: Vec<_> = records
        .iter()
        .filter(|record| {
            record.source == "SchedulerMachine"
                && matches!(record.event, TelemetryEvent::EffectEmitted { .. })
        })
        .collect();
    assert!(
        !run_node_effects.is_empty(),
        "the scheduler must emit at least one EffectEmitted record"
    );
    for record in &run_node_effects {
        assert!(
            record.node_id.is_some(),
            "SchedulerMachine EffectEmitted must carry node_id for RunNode/IntegrateWork"
        );
        assert!(
            record.attempt.is_some(),
            "SchedulerMachine EffectEmitted must carry attempt for RunNode/IntegrateWork"
        );
    }

    let scheduler_state_or_event_records = records.iter().filter(|record| {
        record.source == "SchedulerMachine"
            && matches!(
                record.event,
                TelemetryEvent::StateEntered { .. } | TelemetryEvent::EventReceived { .. }
            )
    });
    for record in scheduler_state_or_event_records {
        assert_eq!(
            record.node_id, None,
            "SchedulerMachine StateEntered/EventReceived describe the whole graph and must stay unstamped"
        );
        assert_eq!(
            record.attempt, None,
            "SchedulerMachine StateEntered/EventReceived describe the whole graph and must stay unstamped"
        );
    }
}
