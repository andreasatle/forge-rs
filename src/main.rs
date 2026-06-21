//! Binary entry point — development harness for exercising the machines.
//!
//! This binary is not the production Forge CLI. It runs the demo machine and
//! several scheduler scenarios through `run_machine` so that the core state
//! machine logic can be verified without a real provider or network.
//!
//! Each `run_demo` call constructs a `RunGraph`, feeds it into `SchedulerMachine`,
//! and prints the final node statuses. The scenarios cover the main recovery
//! paths: serial chains, plan expansion, retry, model escalation, and terminal
//! failure.

use forge_rs::engine::run_machine;
use forge_rs::machines::demo::state::DemoState;
use forge_rs::machines::demo::{DemoMachine, Task};
use forge_rs::machines::scheduler::state::SchedulerState;
use forge_rs::machines::scheduler::{
    ModelTier, Node, NodeId, NodeKind, NodeOrigin, NodeStatus, RunGraph, RunRequest,
    SchedulerHandler, SchedulerMachine, SchedulerOutput,
};
use forge_rs::node_runner::StaticNodeRunner;

fn work(id: &str, objective: &str, deps: &[&str]) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Work,
        objective: objective.to_string(),
        dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
        status: NodeStatus::Pending,
        attempt: 0,
        plan_depth: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
        origin: NodeOrigin::Root,
    }
}

fn run_demo(label: &str, state: SchedulerState) {
    println!("\n=== {label} ===\n");
    match run_machine(SchedulerHandler::new(StaticNodeRunner), state) {
        SchedulerOutput::Complete {
            graph: g,
            recovery_summary: rs,
        } => {
            println!(
                "\nCOMPLETE — {} nodes (recovered={}, retry={}, elevate={}, split={})",
                g.nodes.len(),
                rs.recovered,
                rs.retry_count,
                rs.elevate_count,
                rs.split_count,
            );
            for n in &g.nodes {
                println!("  [{:?}] {} {:?}", n.status, n.id.0, n.summary);
            }
        }
        SchedulerOutput::Failed { graph: g, reason } => {
            println!("\nFAILED: {reason}");
            for n in &g.nodes {
                println!("  [{:?}] {} {:?}", n.status, n.id.0, n.summary);
            }
        }
    }
}

fn main() {
    // Demo machine (unchanged)
    let result = run_machine(
        DemoMachine,
        DemoState::NotStarted {
            task: Task {
                name: "demo task".to_string(),
            },
        },
    );
    println!("DEMO RESULT: {result:#?}");

    // 1. Simple work chain: A → B → C
    run_demo(
        "work chain",
        SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![
                    work("A", "initialize workspace", &[]),
                    work("B", "build artifacts", &["A"]),
                    work("C", "run verification", &["B"]),
                ],
                next_id: 0,
            },
        },
    );

    // 2. Plan node that dynamically creates a work child — primary entry via RunRequest
    run_demo(
        "plan → work child (via RunRequest)",
        SchedulerMachine::initial_state(RunRequest {
            objective: "plan the implementation".to_string(),
        }),
    );

    // 3. A work node that succeeds (StaticNodeRunner: no "fail" keyword)
    run_demo(
        "single work node",
        SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work("R", "run this job", &[])],
                next_id: 0,
            },
        },
    );

    // 4. Terminal failure (StaticNodeRunner: "fail" in objective → Terminal recovery)
    run_demo(
        "terminal failure",
        SchedulerState::Running {
            graph: RunGraph {
                nodes: vec![work("X", "fail this step", &[])],
                next_id: 0,
            },
        },
    );
}
