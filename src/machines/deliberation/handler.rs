//! Provider-backed handler for `DeliberationEffect::RunRole`.
//!
//! Connects `DeliberationMachine` to the provider boundary without touching
//! `SchedulerMachine` or `AgentMachine`. No async, no streaming, no JSON
//! role protocol — just synchronous provider calls and string-prefix parsing.

use crate::providers::{ProviderClient, ProviderRequest};

use super::effect::DeliberationEffect;
use super::event::{DeliberationEvent, RoleResult};
use super::state::{DeliberationRole, RevisionFeedback};

/// Executes `DeliberationEffect` values by calling a `ProviderClient` and
/// mapping the response into `DeliberationEvent` values.
pub struct ProviderBackedDeliberationHandler<P> {
    provider: P,
}

impl<P> ProviderBackedDeliberationHandler<P> {
    /// Wrap a provider in a handler.
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P: ProviderClient> ProviderBackedDeliberationHandler<P> {
    /// Execute one deliberation effect and return the resulting event.
    ///
    /// `ReturnComplete` and `ReturnFailed` are terminal effects: `run_machine`
    /// checks `output()` before dispatching effects, so reaching them here is
    /// a bug in the caller.
    pub fn handle_effect(&self, effect: DeliberationEffect) -> DeliberationEvent {
        match effect {
            DeliberationEffect::RunRole {
                role,
                objective,
                producer_content,
                critic_content,
                feedback,
            } => {
                let prompt = render_role_prompt(
                    &role,
                    &objective,
                    producer_content.as_deref(),
                    critic_content.as_deref(),
                    &feedback,
                );
                let result = match self.provider.call(ProviderRequest { prompt }) {
                    Ok(resp) => parse_role_response(&resp.content),
                    Err(err) => RoleResult::Failed {
                        reason: format!("provider error ({:?}): {}", err.kind, err.message),
                    },
                };
                DeliberationEvent::RoleReturned { role, result }
            }
            DeliberationEffect::ReturnComplete { .. } => {
                unreachable!(
                    "ReturnComplete is a terminal effect; \
                     run_machine returns before dispatching it"
                )
            }
            DeliberationEffect::ReturnFailed { .. } => {
                unreachable!(
                    "ReturnFailed is a terminal effect; \
                     run_machine returns before dispatching it"
                )
            }
        }
    }
}

/// Build a prompt for a single role invocation.
///
/// Includes the objective, role, prior-stage content when present, and any
/// accumulated revision feedback. Kept intentionally simple — no templates.
fn render_role_prompt(
    role: &DeliberationRole,
    objective: &str,
    producer_content: Option<&str>,
    critic_content: Option<&str>,
    feedback: &[RevisionFeedback],
) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Objective: {objective}"));
    parts.push(format!("Role: {role:?}"));
    if let Some(pc) = producer_content {
        parts.push(format!("Producer content: {pc}"));
    }
    if let Some(cc) = critic_content {
        parts.push(format!("Critic content: {cc}"));
    }
    if !feedback.is_empty() {
        let reasons: Vec<&str> = feedback.iter().map(|f| f.reason.as_str()).collect();
        parts.push(format!("Revision feedback: {}", reasons.join("; ")));
    }
    parts.join("\n")
}

/// Parse the raw content string returned by the provider into a `RoleResult`.
///
/// Scans for the first `ACCEPT:` or `REJECT:` marker anywhere in the response
/// so that chain-of-thought preamble does not cause spurious failures.
///
/// - First `ACCEPT:` wins over a later `REJECT:`, and vice-versa.
/// - If neither marker is present → `Failed`.
fn parse_role_response(content: &str) -> RoleResult {
    let accept_pos = content.find("ACCEPT:");
    let reject_pos = content.find("REJECT:");
    match (accept_pos, reject_pos) {
        (Some(a), Some(r)) if a <= r => RoleResult::Accepted {
            content: content[a + 7..].trim().to_string(),
        },
        (Some(_), Some(r)) => RoleResult::Rejected {
            reason: content[r + 7..].trim().to_string(),
        },
        (Some(a), None) => RoleResult::Accepted {
            content: content[a + 7..].trim().to_string(),
        },
        (None, Some(r)) => RoleResult::Rejected {
            reason: content[r + 7..].trim().to_string(),
        },
        (None, None) => RoleResult::Failed {
            reason: format!("malformed role response: {content:?}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::engine::{Machine, Transition, run_machine};
    use crate::machines::deliberation::machine::DeliberationMachine;
    use crate::machines::deliberation::state::{
        DeliberationRequest, DeliberationState, DeliberationTerminalOutput,
    };
    use crate::providers::ProviderRequest;
    use crate::providers::types::{ProviderError, ProviderErrorKind, ProviderResponse};

    // --- fake providers ---

    struct ConstantProvider(String);

    impl ProviderClient for ConstantProvider {
        fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Ok(ProviderResponse {
                content: self.0.clone(),
            })
        }
    }

    struct FailingProvider {
        kind: ProviderErrorKind,
        message: String,
    }

    impl ProviderClient for FailingProvider {
        fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Err(ProviderError {
                kind: self.kind.clone(),
                message: self.message.clone(),
            })
        }
    }

    struct ScriptedProvider {
        responses: RefCell<VecDeque<String>>,
    }

    impl ScriptedProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
            }
        }
    }

    impl ProviderClient for ScriptedProvider {
        fn call(&self, _req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            let content = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedProvider: responses exhausted");
            Ok(ProviderResponse { content })
        }
    }

    // --- Machine wrapper for run_machine tests ---

    struct ProvidedMachine<P: ProviderClient> {
        handler: ProviderBackedDeliberationHandler<P>,
    }

    impl<P: ProviderClient> Machine for ProvidedMachine<P> {
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

    // --- helpers ---

    fn run_role(
        role: DeliberationRole,
        objective: &str,
        producer_content: Option<&str>,
        critic_content: Option<&str>,
        feedback: Vec<RevisionFeedback>,
    ) -> DeliberationEffect {
        DeliberationEffect::RunRole {
            role,
            objective: objective.to_string(),
            producer_content: producer_content.map(|s| s.to_string()),
            critic_content: critic_content.map(|s| s.to_string()),
            feedback,
        }
    }

    fn ready(objective: &str, max_revisions: usize) -> DeliberationState {
        DeliberationState::Ready {
            request: DeliberationRequest {
                objective: objective.to_string(),
                max_revisions,
            },
        }
    }

    // --- tests ---

    #[test]
    fn provider_accept_response_maps_to_role_accepted() {
        let handler =
            ProviderBackedDeliberationHandler::new(ConstantProvider("ACCEPT: draft".to_string()));
        let event = handler.handle_effect(run_role(
            DeliberationRole::Producer,
            "write a poem",
            None,
            None,
            vec![],
        ));
        assert!(
            matches!(
                &event,
                DeliberationEvent::RoleReturned {
                    role: DeliberationRole::Producer,
                    result: RoleResult::Accepted { content },
                } if content == "draft"
            ),
            "expected RoleReturned(Producer, Accepted {{ 'draft' }}), got {event:?}"
        );
    }

    #[test]
    fn provider_reject_response_maps_to_role_rejected() {
        let handler = ProviderBackedDeliberationHandler::new(ConstantProvider(
            "REJECT: needs changes".to_string(),
        ));
        let event = handler.handle_effect(run_role(
            DeliberationRole::Referee,
            "write a poem",
            Some("draft"),
            Some("review"),
            vec![],
        ));
        assert!(
            matches!(
                &event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Rejected { reason },
                    ..
                } if reason == "needs changes"
            ),
            "expected Rejected {{ 'needs changes' }}, got {event:?}"
        );
    }

    #[test]
    fn malformed_provider_response_maps_to_failed() {
        let handler = ProviderBackedDeliberationHandler::new(ConstantProvider("hello".to_string()));
        let event = handler.handle_effect(run_role(
            DeliberationRole::Producer,
            "write a poem",
            None,
            None,
            vec![],
        ));
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Failed { .. },
                    ..
                }
            ),
            "expected Failed for malformed response, got {event:?}"
        );
    }

    #[test]
    fn provider_retryable_error_maps_to_failed() {
        let handler = ProviderBackedDeliberationHandler::new(FailingProvider {
            kind: ProviderErrorKind::Retryable,
            message: "rate limited".to_string(),
        });
        let event = handler.handle_effect(run_role(
            DeliberationRole::Producer,
            "write a poem",
            None,
            None,
            vec![],
        ));
        match &event {
            DeliberationEvent::RoleReturned { result, .. } => {
                assert!(
                    matches!(result, RoleResult::Failed { .. }),
                    "retryable provider error must map to Failed, not {result:?}"
                );
            }
            other => panic!("expected RoleReturned, got {other:?}"),
        }
    }

    #[test]
    fn provider_terminal_error_maps_to_failed() {
        let handler = ProviderBackedDeliberationHandler::new(FailingProvider {
            kind: ProviderErrorKind::Terminal,
            message: "auth error".to_string(),
        });
        let event = handler.handle_effect(run_role(
            DeliberationRole::Producer,
            "write a poem",
            None,
            None,
            vec![],
        ));
        match &event {
            DeliberationEvent::RoleReturned { result, .. } => {
                assert!(
                    matches!(result, RoleResult::Failed { .. }),
                    "terminal provider error must map to Failed, not {result:?}"
                );
            }
            other => panic!("expected RoleReturned, got {other:?}"),
        }
    }

    #[test]
    fn role_prompt_includes_feedback() {
        let feedback = vec![RevisionFeedback {
            reason: "too vague".to_string(),
        }];
        let prompt = render_role_prompt(
            &DeliberationRole::Producer,
            "write a poem",
            None,
            None,
            &feedback,
        );
        assert!(
            prompt.contains("too vague"),
            "expected prompt to include feedback reason 'too vague', got: {prompt}"
        );
        assert!(
            prompt.contains("write a poem"),
            "expected prompt to include objective, got: {prompt}"
        );
    }

    #[test]
    fn run_machine_with_provider_handler_success() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
                "ACCEPT: draft",
                "ACCEPT: review",
                "ACCEPT: approved",
            ])),
        };
        let output = run_machine(machine, ready("write a poem", 0));
        match output {
            DeliberationTerminalOutput::Complete(out) => {
                assert_eq!(out.content, "draft");
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn run_machine_with_provider_handler_revision() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
                "ACCEPT: draft v1",      // Producer call 1
                "ACCEPT: review",        // Critic call 1
                "REJECT: needs changes", // Referee call 1 → revision loop
                "ACCEPT: draft v2",      // Producer call 2
                "ACCEPT: review ok",     // Critic call 2
                "ACCEPT: approved",      // Referee call 2
            ])),
        };
        let output = run_machine(machine, ready("write a poem", 1));
        match output {
            DeliberationTerminalOutput::Complete(out) => {
                assert_eq!(out.content, "draft v2");
            }
            other => panic!("expected Complete with 'draft v2', got {other:?}"),
        }
    }

    #[test]
    fn response_with_preamble_then_accept_parses() {
        let result = parse_role_response("Sure, here is my answer.\nACCEPT: the content");
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "the content"),
            "expected Accepted {{ 'the content' }}, got {result:?}"
        );
    }

    #[test]
    fn response_with_preamble_then_reject_parses() {
        let result = parse_role_response("Let me think about this.\nREJECT: needs more work");
        assert!(
            matches!(result, RoleResult::Rejected { ref reason } if reason == "needs more work"),
            "expected Rejected {{ 'needs more work' }}, got {result:?}"
        );
    }

    #[test]
    fn response_with_both_markers_uses_first() {
        let result = parse_role_response("REJECT: bad ACCEPT: good");
        assert!(
            matches!(result, RoleResult::Rejected { ref reason } if reason == "bad ACCEPT: good"),
            "expected Rejected when REJECT: precedes ACCEPT:, got {result:?}"
        );

        let result = parse_role_response("ACCEPT: first REJECT: second");
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "first REJECT: second"),
            "expected Accepted when ACCEPT: precedes REJECT:, got {result:?}"
        );
    }

    #[test]
    fn response_without_marker_still_fails() {
        let result = parse_role_response("I have no idea what you want.");
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "expected Failed when no marker present, got {result:?}"
        );
    }
}
