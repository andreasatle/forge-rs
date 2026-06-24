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
use forge_rs::providers::{LlamaCppProvider, RetryingProvider};
use forge_rs::telemetry::FileTelemetry;
use std::path::PathBuf;

/// Machine wrapper that connects DeliberationMachine to the provider handler.
struct LiveMachine<P: forge_rs::providers::ProviderClient> {
    handler: ProviderBackedDeliberationHandler<P>,
}

impl<P: forge_rs::providers::ProviderClient> Machine for LiveMachine<P> {
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

    let llama = LlamaCppProvider::new("http://localhost:8080", 120);
    let retrying = RetryingProvider::new(llama, 3);
    let handler = ProviderBackedDeliberationHandler::new(retrying);
    let machine = LiveMachine { handler };

    let initial = DeliberationState::Ready {
        request: DeliberationRequest {
            objective: objective.to_string(),
            max_revisions,
        },
    };

    let telemetry_dir = PathBuf::from("runs/latest");
    let _ = std::fs::remove_dir_all(&telemetry_dir);
    let sink = FileTelemetry::new(telemetry_dir.clone());

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
