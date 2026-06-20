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
    ModelTier, Node, NodeId, NodeKind, NodeStatus, RunGraph, SchedulerMachine, SchedulerOutput,
};

fn work(id: &str, objective: &str, deps: &[&str]) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Work,
        objective: objective.to_string(),
        dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
        status: NodeStatus::Pending,
        attempt: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
    }
}

fn plan(id: &str, objective: &str, deps: &[&str]) -> Node {
    Node {
        id: NodeId(id.to_string()),
        kind: NodeKind::Plan,
        objective: objective.to_string(),
        dependencies: deps.iter().map(|d| NodeId(d.to_string())).collect(),
        status: NodeStatus::Pending,
        attempt: 0,
        model_tier: ModelTier::Cheap,
        summary: None,
    }
}

fn run_demo(label: &str, graph: RunGraph) {
    println!("\n=== {label} ===\n");
    match run_machine(SchedulerMachine, SchedulerState::Running { graph }) {
        SchedulerOutput::Complete(g) => {
            println!("\nCOMPLETE — {} nodes", g.nodes.len());
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
        RunGraph {
            nodes: vec![
                work("A", "initialize workspace", &[]),
                work("B", "build artifacts", &["A"]),
                work("C", "run verification", &["B"]),
            ],
            next_id: 0,
        },
    );

    // 2. Plan node that dynamically creates a work child
    run_demo(
        "plan → work child",
        RunGraph {
            nodes: vec![plan("P", "plan the implementation", &[])],
            next_id: 0,
        },
    );

    // 3. Retry: fails on attempt 0, succeeds on attempt 1
    run_demo(
        "retry recovery",
        RunGraph {
            nodes: vec![work("R", "retry this job", &[])],
            next_id: 0,
        },
    );

    // 4. ElevateModel: fails on attempt 0, succeeds on attempt 1 with Strong tier
    run_demo(
        "elevate model recovery",
        RunGraph {
            nodes: vec![work("E", "elevate this task", &[])],
            next_id: 0,
        },
    );

    // 5. Terminal failure
    run_demo(
        "terminal failure",
        RunGraph {
            nodes: vec![work("X", "terminal task", &[])],
            next_id: 0,
        },
    );
}
