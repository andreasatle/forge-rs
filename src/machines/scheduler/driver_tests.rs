use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
