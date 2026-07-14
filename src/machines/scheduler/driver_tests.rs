use super::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::machines::scheduler::{
    FailureReason, ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunConfig, RunGraph,
    WorkOutput,
};
use crate::node_runner::NodeRunner;
use crate::node_runner::types::{NodeRunRequest, NodeRunResult, NodeRunWorkResult};
use crate::runtime::ResourceManager;
use crate::telemetry::TelemetrySink;

fn work_node(id: &str) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Work,
        team: String::new(),
        task_id: None,
        adapter: String::new(),
        northstar: String::new(),
        worker_role: None,
        objective: format!("do {id}"),
        target_files: vec![],
        required_validation_targets: vec![],
        dependencies: vec![],
        status: NodeStatus::Pending,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
        validation_plan: None,
        retry_feedback: None,
    }
}

fn work_accepted() -> NodeRunResult {
    NodeRunResult::WorkAccepted(NodeRunWorkResult {
        work: WorkOutput {
            summary: "done".to_string(),
        },
    })
}

/// Sleeps for `delay` on every dispatch, tracking how many calls were ever
/// concurrently in progress.
struct DelayedRunner {
    delay: Duration,
    active: Arc<AtomicUsize>,
    max_observed: Arc<AtomicUsize>,
}

impl NodeRunner for DelayedRunner {
    fn run_node(&self, _request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_observed.fetch_max(now_active, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.active.fetch_sub(1, Ordering::SeqCst);
        work_accepted()
    }
}

// Invariant: with `dispatch_cap` >= the number of ready nodes, the scheduler
// dispatches them onto separate threads that run genuinely in parallel, not
// merely interleaved — two nodes that each sleep for `delay` complete in
// roughly one `delay`, not two.
#[test]
fn two_nodes_with_dispatch_cap_two_run_concurrently_not_serially() {
    let delay = Duration::from_millis(200);
    let active = Arc::new(AtomicUsize::new(0));
    let max_observed = Arc::new(AtomicUsize::new(0));
    let runner = DelayedRunner {
        delay,
        active: Arc::clone(&active),
        max_observed: Arc::clone(&max_observed),
    };

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("A"), work_node("B")],
        },
        run_config: RunConfig {
            dispatch_cap: 2,
            ..RunConfig::default()
        },
    };

    let start = Instant::now();
    let output = run_scheduler(SchedulerHandler::new(runner), state);
    let elapsed = start.elapsed();

    assert!(
        matches!(output, SchedulerTerminalOutput::Complete { .. }),
        "expected Complete, got {output:#?}"
    );
    assert_eq!(
        max_observed.load(Ordering::SeqCst),
        2,
        "both nodes must have been dispatched at the same time at some point"
    );
    assert!(
        elapsed < delay * 3 / 2,
        "two concurrent {delay:?} dispatches should take about one delay, took {elapsed:?}"
    );
}

/// Records, per node id, the elapsed time (since a shared `start`) at which
/// its dispatch began, then sleeps for that node's configured delay.
struct TimestampingRunner {
    start: Instant,
    delays: HashMap<String, Duration>,
    dispatch_starts: Arc<Mutex<HashMap<String, Duration>>>,
}

impl NodeRunner for TimestampingRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let elapsed = self.start.elapsed();
        self.dispatch_starts
            .lock()
            .unwrap()
            .insert(request.node_id.0.clone(), elapsed);
        std::thread::sleep(self.delays[&request.node_id.0]);
        work_accepted()
    }
}

// Invariant: dispatch is opportunistic, not wave-gated. With `dispatch_cap`
// 2 and three ready nodes, the two most-recently-inserted (A and B) dispatch
// immediately and the oldest (C) stays Pending (no free slot). Once A (fast)
// completes, its freed slot must be backfilled with C right away — C must
// not sit idle until B (slow) also completes.
#[test]
fn freed_slot_is_backfilled_as_soon_as_the_fast_node_completes_not_waiting_for_the_slow_one() {
    let fast = Duration::from_millis(150);
    let slow = Duration::from_millis(600);
    let start = Instant::now();
    let dispatch_starts = Arc::new(Mutex::new(HashMap::new()));
    let delays = HashMap::from([
        ("A".to_string(), fast),
        ("B".to_string(), slow),
        ("C".to_string(), fast),
    ]);
    let runner = TimestampingRunner {
        start,
        delays,
        dispatch_starts: Arc::clone(&dispatch_starts),
    };

    // `find_ready` prefers the most-recently-inserted ready node, so C (the
    // node meant to be held back) is placed first in the vec and A/B (meant
    // to dispatch immediately) after it.
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("C"), work_node("A"), work_node("B")],
        },
        run_config: RunConfig {
            dispatch_cap: 2,
            ..RunConfig::default()
        },
    };

    let output = run_scheduler(SchedulerHandler::new(runner), state);
    assert!(
        matches!(output, SchedulerTerminalOutput::Complete { .. }),
        "expected Complete, got {output:#?}"
    );

    let starts = dispatch_starts.lock().unwrap();
    let a_start = starts["A"];
    let b_start = starts["B"];
    let c_start = *starts
        .get("C")
        .expect("C must have been dispatched at some point");

    assert!(
        a_start < Duration::from_millis(100) && b_start < Duration::from_millis(100),
        "A and B must both dispatch immediately at the start of the run: \
         a_start={a_start:?}, b_start={b_start:?}"
    );
    assert!(
        c_start >= a_start + fast,
        "C must not start before A actually completes: c_start={c_start:?}, \
         a_start={a_start:?}, fast={fast:?}"
    );
    assert!(
        c_start < b_start + slow / 2,
        "C must be backfilled into A's freed slot well before B completes, \
         not wait for the whole batch to drain: c_start={c_start:?}, \
         b_start={b_start:?}, slow={slow:?}"
    );
}

/// Panics on every dispatch.
struct PanickingRunner;

impl NodeRunner for PanickingRunner {
    fn run_node(&self, _request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        panic!("boom: deliberate dispatch-thread panic for test coverage");
    }
}

// Invariant: a panic inside a node's dispatch thread is caught and converted
// into a NodeFailed event (routed through the same recovery machinery as any
// other failure) instead of unwinding out of `run_scheduler` and taking the
// whole process down with it.
#[test]
fn panic_in_dispatch_thread_becomes_a_node_failed_event_not_a_crash() {
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("W")],
        },
        run_config: RunConfig::default(),
    };

    let output = run_scheduler(SchedulerHandler::new(PanickingRunner), state);

    let SchedulerTerminalOutput::Failed { graph, reason } = output else {
        panic!("expected Failed after a dispatch-thread panic, got {output:#?}");
    };
    assert!(
        matches!(reason, FailureReason::TerminalRecovery { .. }),
        "a dispatch-thread panic must route through Terminal recovery; got {reason:?}"
    );
    assert_eq!(graph.nodes[0].status, NodeStatus::Failed);
}

/// Acquires a shared [`ResourceManager`] permit for the duration of the
/// (simulated) provider call, standing in for `ResourceGatedProvider`
/// wrapping an actual `ProviderClient`.
struct ResourceGatedRunner {
    resource_manager: ResourceManager,
    active: Arc<AtomicUsize>,
    max_observed: Arc<AtomicUsize>,
    delay: Duration,
}

impl NodeRunner for ResourceGatedRunner {
    fn run_node(&self, _request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        let _permit = self.resource_manager.acquire();
        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_observed.fetch_max(now_active, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.active.fetch_sub(1, Ordering::SeqCst);
        work_accepted()
    }
}

// Invariant: `dispatch_cap` and a provider's `ResourceManager` are separate
// knobs. Three nodes can be dispatched (and count as in-flight) at once, but
// a `ResourceManager` with only one permit still forces their (simulated)
// provider calls to run one at a time — the gate genuinely blocks callers
// past its capacity rather than letting everything through immediately.
#[test]
fn resource_manager_serializes_provider_calls_across_concurrently_dispatched_nodes() {
    let resource_manager = ResourceManager::new(1);
    let active = Arc::new(AtomicUsize::new(0));
    let max_observed = Arc::new(AtomicUsize::new(0));
    let runner = ResourceGatedRunner {
        resource_manager,
        active: Arc::clone(&active),
        max_observed: Arc::clone(&max_observed),
        delay: Duration::from_millis(80),
    };

    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("A"), work_node("B"), work_node("C")],
        },
        run_config: RunConfig {
            dispatch_cap: 3,
            ..RunConfig::default()
        },
    };

    let output = run_scheduler(SchedulerHandler::new(runner), state);

    assert!(
        matches!(output, SchedulerTerminalOutput::Complete { .. }),
        "expected Complete, got {output:#?}"
    );
    assert_eq!(
        max_observed.load(Ordering::SeqCst),
        1,
        "a ResourceManager with 1 permit must serialize gated work even though \
         3 nodes were dispatched concurrently"
    );
}

/// Node "A" fails Terminal immediately; node "B" runs slowly (already
/// dispatched alongside A in the same batch); node "C" is a third ready node
/// held back only by `dispatch_cap`, meant to be backfilled once a slot
/// frees — unless the run has already ended.
struct TerminalCancellationRunner {
    b_delay: Duration,
    b_completed: Arc<AtomicBool>,
    c_dispatched: Arc<AtomicBool>,
}

impl NodeRunner for TerminalCancellationRunner {
    fn run_node(&self, request: NodeRunRequest, _telemetry: &dyn TelemetrySink) -> NodeRunResult {
        match request.node_id.0.as_str() {
            "A" => NodeRunResult::Failed(NodeFailure {
                kind: FailureKind::ProviderTerminalFailure,
                message: "fatal provider error".to_string(),
                recovery: RecoveryAction::Terminal {
                    message: "unrecoverable".to_string(),
                },
            }),
            "B" => {
                std::thread::sleep(self.b_delay);
                self.b_completed.store(true, Ordering::SeqCst);
                work_accepted()
            }
            "C" => {
                self.c_dispatched.store(true, Ordering::SeqCst);
                work_accepted()
            }
            other => panic!("unexpected node dispatched: {other}"),
        }
    }
}

// Invariant: once a Terminal recovery decides the run's outcome, the driver
// must not emit any further RunNode dispatch for other still-pending/eligible
// nodes (here, C — held back only by `dispatch_cap`, otherwise ready) — while
// already-dispatched siblings from the same batch (here, B, dispatched
// alongside the node that fails) still run to completion and get joined
// before `run_scheduler` returns.
#[test]
fn terminal_recovery_stops_new_dispatch_but_joins_already_inflight_siblings() {
    let b_completed = Arc::new(AtomicBool::new(false));
    let c_dispatched = Arc::new(AtomicBool::new(false));
    let runner = TerminalCancellationRunner {
        b_delay: Duration::from_millis(200),
        b_completed: Arc::clone(&b_completed),
        c_dispatched: Arc::clone(&c_dispatched),
    };

    // `find_ready` iterates nodes in reverse, so with this ordering A and B
    // dispatch immediately (dispatch_cap == 2) and C is held back for a
    // freed slot that should never come.
    let state = SchedulerState::Active {
        graph: RunGraph {
            nodes: vec![work_node("C"), work_node("A"), work_node("B")],
        },
        run_config: RunConfig {
            dispatch_cap: 2,
            ..RunConfig::default()
        },
    };

    let output = run_scheduler(SchedulerHandler::new(runner), state);

    let SchedulerTerminalOutput::Failed { graph, reason } = output else {
        panic!("expected Failed after a Terminal recovery, got {output:#?}");
    };
    assert!(
        matches!(reason, FailureReason::TerminalRecovery { .. }),
        "expected TerminalRecovery, got {reason:?}"
    );
    assert!(
        !c_dispatched.load(Ordering::SeqCst),
        "C must never be dispatched once the run's outcome is already Failed"
    );
    assert_eq!(
        graph.nodes.iter().find(|n| n.id.0 == "C").unwrap().status,
        NodeStatus::Pending,
        "C must remain untouched, having never been dispatched or cancelled"
    );
    assert!(
        b_completed.load(Ordering::SeqCst),
        "B was already dispatched alongside A and must still run to \
         completion and be joined before run_scheduler returns"
    );
}
