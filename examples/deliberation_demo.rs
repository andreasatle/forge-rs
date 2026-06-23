//! Live deliberation demo — Producer → Critic → Referee via local llama-server.
//!
//! Start the server with:
//!     llama-server -hf lm-kit/qwen-3-8b-instruct-gguf:Q4_K_M --temp 0
//!
//! Usage:
//!     cargo run --example deliberation_demo
//!
//! Requires llama-server running at http://localhost:8080.

use forge_rs::engine::{Machine, Transition, run_machine_with_telemetry};
use forge_rs::machines::deliberation::{
    DeliberationEffect, DeliberationEvent, DeliberationMachine, DeliberationRequest,
    DeliberationState, DeliberationTerminalOutput, ProviderBackedDeliberationHandler,
};
use forge_rs::providers::{
    LlamaCppProvider, ProviderClient, ProviderError, ProviderRequest, ProviderResponse,
    RetryingProvider,
};
use forge_rs::telemetry::FileTelemetry;
use std::path::PathBuf;

// Prepended before the task prompt.
const PROTOCOL_PREFIX: &str = "\
Return exactly one JSON object. No markdown. No code fence. No explanation.\n\
No text before or after the JSON.\n\
Accepted schema: {\"status\":\"accepted\",\"content\":\"...\"}\n\
Rejected schema: {\"status\":\"rejected\",\"reason\":\"...\"}";

// Appended after the task prompt.
const PROTOCOL_SUFFIX: &str = "\n\
Return exactly one JSON object. No markdown. No code fence. No explanation.\n\
Your response must be valid JSON with \"status\" set to \"accepted\" or \"rejected\".";

/// Wraps any provider and sandwiches the task prompt between the protocol
/// instructions so that both the top and bottom of the context reinforce
/// the expected output format.
struct InstructedProvider<P> {
    inner: P,
}

impl<P: ProviderClient> ProviderClient for InstructedProvider<P> {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let wrapped = format!(
            "{}\n\n{}\n\n{}",
            PROTOCOL_PREFIX, req.prompt, PROTOCOL_SUFFIX
        );
        self.inner.call(ProviderRequest {
            prompt: wrapped,
            max_tokens: req.max_tokens,
            output_schema: req.output_schema,
        })
    }
}

/// Machine wrapper that connects DeliberationMachine to the provider handler.
struct LiveMachine<P: ProviderClient> {
    handler: ProviderBackedDeliberationHandler<P>,
}

impl<P: ProviderClient> Machine for LiveMachine<P> {
    type State = DeliberationState;
    type Event = DeliberationEvent;
    type Effect = DeliberationEffect;
    type Output = DeliberationTerminalOutput;

    fn start_event(&self) -> DeliberationEvent {
        DeliberationEvent::Start
    }

    fn transition(
        &self,
        state: DeliberationState,
        event: DeliberationEvent,
    ) -> Transition<DeliberationState, DeliberationEffect> {
        DeliberationMachine.transition(state, event)
    }

    fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        self.handler.handle_effect(effect)
    }

    fn output(&self, state: &DeliberationState) -> Option<DeliberationTerminalOutput> {
        DeliberationMachine.output(state)
    }
}

fn main() {
    let objective = "Write a short haiku about Rust state machines.";
    let max_revisions = 1;

    println!("Objective : {objective}");
    println!("Max revisions: {max_revisions}");
    println!();

    let llama = LlamaCppProvider::new("http://localhost:8080");
    let retrying = RetryingProvider::new(llama, 3);
    let instructed = InstructedProvider { inner: retrying };
    let handler = ProviderBackedDeliberationHandler::new(instructed);
    let machine = LiveMachine { handler };

    let initial = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: objective.to_string(),
            max_revisions,
        },
    };

    let telemetry_dir = PathBuf::from("runs/latest");
    let _ = std::fs::remove_dir_all(&telemetry_dir);
    let sink = FileTelemetry::new(telemetry_dir.clone()).expect("failed to create telemetry dir");

    match run_machine_with_telemetry(machine, initial, &sink).0 {
        DeliberationTerminalOutput::Complete(out) => {
            println!("COMPLETE");
            println!("{}", out.content);
        }
        DeliberationTerminalOutput::Failed { reason } => {
            println!("FAILED: {reason}");
        }
    }

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
                finish_reason: None,
            })
        }
    }

    #[test]
    fn instructed_provider_contains_json_protocol() {
        let provider = InstructedProvider {
            inner: EchoProvider,
        };
        let resp = provider
            .call(ProviderRequest {
                prompt: "base prompt".to_string(),
                max_tokens: 512,
                output_schema: None,
            })
            .unwrap();
        assert!(
            resp.content.contains("base prompt"),
            "original prompt must be preserved"
        );
        assert!(
            resp.content.contains("\"status\""),
            "wrapped prompt must contain JSON status field instruction"
        );
        assert!(
            resp.content.contains("\"accepted\""),
            "wrapped prompt must contain accepted schema"
        );
        assert!(
            resp.content.contains("\"rejected\""),
            "wrapped prompt must contain rejected schema"
        );
    }

    #[test]
    fn instructed_provider_wraps_prompt() {
        let provider = InstructedProvider {
            inner: EchoProvider,
        };
        let resp = provider
            .call(ProviderRequest {
                prompt: "my prompt".to_string(),
                max_tokens: 512,
                output_schema: None,
            })
            .unwrap();
        let pos_prompt = resp.content.find("my prompt").unwrap();
        let pos_prefix = resp.content.find(PROTOCOL_PREFIX).unwrap();
        let pos_suffix = resp.content.rfind(PROTOCOL_SUFFIX).unwrap();
        assert!(
            pos_prefix < pos_prompt,
            "protocol prefix must precede the task prompt"
        );
        assert!(
            pos_prompt < pos_suffix,
            "protocol suffix must follow the task prompt"
        );
    }
}
