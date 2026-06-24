//! Live scheduler → deliberation demo.
//!
//! Wires:
//!   SchedulerMachine
//!     → SchedulerHandler
//!     → DeliberatingNodeRunner
//!     → ProviderBackedDeliberationHandler
//!     → RetryingProvider
//!     → LlamaCppProvider
//!     → llama-server (http://localhost:8080)
//!
//! Start the server with:
//!     llama-server -hf lm-kit/qwen-3-8b-instruct-gguf:Q4_K_M --temp 0
//!
//! Usage:
//!     cargo run --example scheduler_deliberation_demo
//!
//! Requires llama-server running at http://localhost:8080.

use forge_rs::engine::run_machine_with_telemetry;
use forge_rs::machines::scheduler::{
    RunRequest, SchedulerHandler, SchedulerMachine, SchedulerOutput,
};
use forge_rs::node_runner::DeliberatingNodeRunner;
use forge_rs::providers::{LlamaCppProvider, RetryingProvider};
use forge_rs::telemetry::FileTelemetry;
use std::path::PathBuf;

fn print_output(output: SchedulerOutput) {
    match output {
        SchedulerOutput::Complete {
            graph,
            recovery_summary,
        } => {
            println!("Result      : COMPLETE");
            println!(
                "Recovery    : recovered={} retry={} elevate={} split={}",
                recovery_summary.recovered,
                recovery_summary.retry_count,
                recovery_summary.elevate_count,
                recovery_summary.split_count,
            );
            println!("Node count  : {}", graph.nodes.len());
            for node in &graph.nodes {
                let summary = node
                    .summary
                    .as_deref()
                    .map(|s| format!(" | {s}"))
                    .unwrap_or_default();
                println!(
                    "  [{:?}] {} ({:?}){summary}",
                    node.status, node.id.0, node.kind,
                );
            }
        }
        SchedulerOutput::Failed { graph, reason } => {
            println!("Result      : FAILED");
            println!("Reason      : {reason}");
            println!("Node count  : {}", graph.nodes.len());
            for node in &graph.nodes {
                println!("  [{:?}] {} ({:?})", node.status, node.id.0, node.kind,);
            }
        }
    }
}

fn main() {
    let objective = "Write a short haiku about Rust state machines.";

    println!("Objective   : {objective}");
    println!();

    let llama = LlamaCppProvider::new("http://localhost:8080");
    let retrying = RetryingProvider::new(llama, 3);
    let runner = DeliberatingNodeRunner::new(retrying);
    let handler = SchedulerHandler::new(runner);

    let initial_state = SchedulerMachine::initial_state(RunRequest {
        objective: objective.to_string(),
    });

    let telemetry_dir = PathBuf::from("runs/latest");
    let _ = std::fs::remove_dir_all(&telemetry_dir);
    let sink = FileTelemetry::new(telemetry_dir.clone());

    let (output, _handler) = run_machine_with_telemetry(handler, initial_state, &sink);
    print_output(output);

    println!();
    println!("Telemetry written to: runs/latest");
}
