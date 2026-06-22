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
use forge_rs::providers::{
    LlamaCppProvider, ProviderClient, ProviderError, ProviderRequest, ProviderResponse,
    RetryingProvider,
};
use forge_rs::telemetry::FileTelemetry;
use std::path::PathBuf;

const PROTOCOL_PREFIX: &str = "\
Return exactly one JSON object. No markdown. No code fence. No explanation.\n\
No text before or after the JSON.\n\
Accepted schema: {\"status\":\"accepted\",\"content\":\"...\"}\n\
Rejected schema: {\"status\":\"rejected\",\"reason\":\"...\"}";

const PROTOCOL_SUFFIX: &str = "\n\
Return exactly one JSON object. No markdown. No code fence. No explanation.\n\
Your response must be valid JSON with \"status\" set to \"accepted\" or \"rejected\".";

/// Wraps any provider and sandwiches the task prompt between protocol
/// instructions so that both the top and bottom of the context reinforce
/// the expected JSON output format.
struct InstructedProvider<P> {
    inner: P,
}

impl<P: ProviderClient> ProviderClient for InstructedProvider<P> {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let wrapped = format!(
            "{}\n\n{}\n\n{}",
            PROTOCOL_PREFIX, req.prompt, PROTOCOL_SUFFIX
        );
        self.inner.call(ProviderRequest { prompt: wrapped })
    }
}

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

    let llama = LlamaCppProvider::new("http://localhost:8080").with_n_predict(512);
    let retrying = RetryingProvider::new(llama, 3);
    let instructed = InstructedProvider { inner: retrying };
    let runner = DeliberatingNodeRunner::new(instructed);
    let handler = SchedulerHandler::new(runner);

    let initial_state = SchedulerMachine::initial_state(RunRequest {
        objective: objective.to_string(),
    });

    let telemetry_dir = PathBuf::from("runs/latest");
    let _ = std::fs::remove_dir_all(&telemetry_dir);
    let sink = FileTelemetry::new(telemetry_dir.clone()).expect("failed to create telemetry dir");

    let (output, _handler) = run_machine_with_telemetry(handler, initial_state, &sink);
    print_output(output);

    println!();
    println!("Telemetry written to: runs/latest");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoProvider;

    impl ProviderClient for EchoProvider {
        fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Ok(ProviderResponse {
                content: req.prompt,
            })
        }
    }

    #[test]
    fn instructed_provider_preserves_original_prompt() {
        let provider = InstructedProvider {
            inner: EchoProvider,
        };
        let resp = provider
            .call(ProviderRequest {
                prompt: "base prompt".to_string(),
            })
            .unwrap();
        assert!(
            resp.content.contains("base prompt"),
            "original prompt must be preserved"
        );
    }

    #[test]
    fn instructed_provider_contains_json_protocol() {
        let provider = InstructedProvider {
            inner: EchoProvider,
        };
        let resp = provider
            .call(ProviderRequest {
                prompt: "task".to_string(),
            })
            .unwrap();
        assert!(resp.content.contains("\"status\""));
        assert!(resp.content.contains("\"accepted\""));
        assert!(resp.content.contains("\"rejected\""));
    }

    #[test]
    fn instructed_provider_wraps_prompt_with_prefix_and_suffix() {
        let provider = InstructedProvider {
            inner: EchoProvider,
        };
        let resp = provider
            .call(ProviderRequest {
                prompt: "my task".to_string(),
            })
            .unwrap();
        let pos_prompt = resp.content.find("my task").unwrap();
        let pos_prefix = resp.content.find(PROTOCOL_PREFIX).unwrap();
        let pos_suffix = resp.content.rfind(PROTOCOL_SUFFIX).unwrap();
        assert!(pos_prefix < pos_prompt, "prefix must precede the prompt");
        assert!(pos_prompt < pos_suffix, "suffix must follow the prompt");
    }
}
