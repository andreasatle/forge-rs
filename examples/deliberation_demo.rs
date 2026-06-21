//! Live deliberation demo — Producer → Critic → Referee via local llama-server.
//!
//! Usage:
//!     cargo run --example deliberation_demo
//!
//! Requires llama-server running at http://localhost:8080.

use forge_rs::engine::{Machine, Transition, run_machine};
use forge_rs::machines::deliberation::{
    DeliberationEffect, DeliberationEvent, DeliberationMachine, DeliberationRequest,
    DeliberationState, DeliberationTerminalOutput, ProviderBackedDeliberationHandler,
};
use forge_rs::providers::{
    LlamaCppProvider, ProviderClient, ProviderError, ProviderRequest, ProviderResponse,
    RetryingProvider,
};

// Appended to every prompt so the local model knows what prefix to use.
const PROTOCOL_SUFFIX: &str = "\n\n\
Reply with exactly one of these two forms and nothing else before the prefix:\n\
  ACCEPT: <your response>\n\
  REJECT: <reason>\n\
Do not use markdown. Do not add any preamble before ACCEPT: or REJECT:.";

/// Wraps any provider and appends the ACCEPT/REJECT protocol instruction.
struct InstructedProvider<P> {
    inner: P,
}

impl<P: ProviderClient> ProviderClient for InstructedProvider<P> {
    fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let extended = format!("{}{}", req.prompt, PROTOCOL_SUFFIX);
        self.inner.call(ProviderRequest { prompt: extended })
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

    let llama = LlamaCppProvider::new("http://localhost:8080").with_n_predict(256);
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

    match run_machine(machine, initial) {
        DeliberationTerminalOutput::Complete(out) => {
            println!("COMPLETE");
            println!("{}", out.content);
        }
        DeliberationTerminalOutput::Failed { reason } => {
            println!("FAILED: {reason}");
        }
    }
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
    fn instructed_provider_appends_protocol_suffix() {
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
        assert!(
            resp.content.contains("ACCEPT:"),
            "protocol suffix must be appended"
        );
        assert!(
            resp.content.contains("REJECT:"),
            "protocol suffix must include REJECT:"
        );
    }

    #[test]
    fn instructed_provider_suffix_follows_prompt() {
        let provider = InstructedProvider {
            inner: EchoProvider,
        };
        let resp = provider
            .call(ProviderRequest {
                prompt: "my prompt".to_string(),
            })
            .unwrap();
        let pos_prompt = resp.content.find("my prompt").unwrap();
        let pos_accept = resp.content.find("ACCEPT:").unwrap();
        assert!(
            pos_prompt < pos_accept,
            "original prompt must appear before the protocol suffix"
        );
    }
}
