use forge_rs::machines::demo::state::DemoState;
use forge_rs::machines::demo::{DemoMachine, Task};
use forge_rs::machines::scheduler::state::SchedulerState;
use forge_rs::machines::scheduler::{
    Node, NodeId, NodeStatus, RunGraph, SchedulerMachine, SchedulerOutput,
};
use forge_rs::engine::run_machine;

fn main() {
    let task = Task {
        name: "demo task".to_string(),
    };

    let result = run_machine(DemoMachine, DemoState::NotStarted { task });

    println!("DEMO FINAL RESULT:\n{result:#?}");

    println!("\n--- Scheduler Demo ---\n");

    let graph = RunGraph {
        nodes: vec![
            Node {
                id: NodeId("A".to_string()),
                dependencies: vec![],
                status: NodeStatus::Pending,
            },
            Node {
                id: NodeId("B".to_string()),
                dependencies: vec![NodeId("A".to_string())],
                status: NodeStatus::Pending,
            },
            Node {
                id: NodeId("C".to_string()),
                dependencies: vec![NodeId("B".to_string())],
                status: NodeStatus::Pending,
            },
        ],
    };

    let output = run_machine(SchedulerMachine, SchedulerState::NotStarted { graph });

    match output {
        SchedulerOutput::Complete(graph) => {
            println!("SCHEDULER COMPLETE:\n{graph:#?}");
        }
        SchedulerOutput::Failed { graph, reason } => {
            println!("SCHEDULER FAILED: {reason}\n{graph:#?}");
        }
    }
}
