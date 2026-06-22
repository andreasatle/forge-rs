//! Provider-backed handler for `DeliberationEffect::RunRole`.
//!
//! Connects `DeliberationMachine` to the provider boundary. The provider
//! returns raw text; this handler extracts and parses a structured JSON role
//! response before surfacing a `RoleResult` to the machine layer above.

use serde::Deserialize;

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

/// Internal serde type for JSON role responses from the provider.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum JsonRoleResponse {
    Accepted { content: String },
    Rejected { reason: String },
}

/// Build a prompt for a single role invocation.
///
/// Includes the objective, role, prior-stage content when present, any
/// accumulated revision feedback, and the required JSON output schema.
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
    parts.push(
        "Return exactly one JSON object. No markdown. No code fence. No explanation. \
         No text before or after the JSON.\n\
         Accepted: {\"status\":\"accepted\",\"content\":\"...\"}\n\
         Rejected: {\"status\":\"rejected\",\"reason\":\"...\"}\n\
         Producer returns accepted content. \
         Critic accepts with a review or rejects with a reason. \
         Referee accepts approval or rejects with revision feedback. \
         Execution failures are handled by the framework, not the model."
            .to_string(),
    );
    parts.join("\n")
}

/// Parse the raw content string returned by the provider into a `RoleResult`.
///
/// Steps:
/// 1. Trim whitespace.
/// 2. Strip optional markdown code fence (` ```json ` / ` ``` `).
/// 3. Extract the first JSON object (between outermost `{` and `}`).
/// 4. Parse with serde_json into `JsonRoleResponse`.
/// 5. Validate that required string fields are non-empty and not the `"..."` schema
///    placeholder that models sometimes echo back literally from the prompt template.
/// 6. Map any parse or validation failure to `RoleResult::Failed`.
fn parse_role_response(raw_response: &str) -> RoleResult {
    let text = strip_code_fence(raw_response.trim());
    let json_str = match extract_json_object(text) {
        Some(s) => s,
        None => {
            return RoleResult::Failed {
                reason: format!("no JSON object found in role response: {raw_response:?}"),
            };
        }
    };
    match serde_json::from_str::<JsonRoleResponse>(json_str) {
        Ok(JsonRoleResponse::Accepted { content }) => {
            if content.trim().is_empty() {
                RoleResult::Failed {
                    reason: "accepted response has empty content".to_string(),
                }
            } else if content.trim() == "..." {
                RoleResult::Failed {
                    reason: format!(
                        "role response has placeholder accepted content; raw: {raw_response}"
                    ),
                }
            } else {
                RoleResult::Accepted { content }
            }
        }
        Ok(JsonRoleResponse::Rejected { reason }) => {
            if reason.trim().is_empty() || reason.trim() == "..." {
                RoleResult::Failed {
                    reason: format!("role response has placeholder reason; raw: {raw_response}"),
                }
            } else {
                RoleResult::Rejected { reason }
            }
        }
        Err(err) => RoleResult::Failed {
            reason: format!("JSON parse error: {err}"),
        },
    }
}

/// Strip a leading ` ```json ` or ` ``` ` fence and its matching closing ` ``` `.
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    let after_open = if let Some(rest) = s.strip_prefix("```json") {
        rest
    } else if let Some(rest) = s.strip_prefix("```") {
        rest
    } else {
        return s;
    };
    let after_newline = after_open.trim_start_matches('\r').trim_start_matches('\n');
    if let Some(body) = after_newline.strip_suffix("```") {
        body.trim()
    } else {
        after_newline.trim()
    }
}

/// Extract the first balanced JSON object from `s`.
///
/// Starts at the first `{`, then scans forward tracking brace depth while
/// respecting string literals (including `\"` escapes) so that braces inside
/// strings do not affect the depth count. Returns the substring from the
/// opening `{` through its matching `}`, ignoring any trailing text.
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => {
                i += 2; // skip escaped character
                continue;
            }
            b'"' => {
                in_string = !in_string;
            }
            b'{' if !in_string => {
                depth += 1;
            }
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
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

    // --- parse_role_response unit tests ---

    #[test]
    fn json_accepted_response_maps_to_role_accepted() {
        let result = parse_role_response(r#"{"status":"accepted","content":"draft"}"#);
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "draft"),
            "expected Accepted {{ 'draft' }}, got {result:?}"
        );
    }

    #[test]
    fn json_rejected_response_maps_to_role_rejected() {
        let result = parse_role_response(r#"{"status":"rejected","reason":"needs changes"}"#);
        assert!(
            matches!(result, RoleResult::Rejected { ref reason } if reason == "needs changes"),
            "expected Rejected {{ 'needs changes' }}, got {result:?}"
        );
    }

    #[test]
    fn json_accepted_empty_content_fails() {
        let result = parse_role_response(r#"{"status":"accepted","content":""}"#);
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "empty content must produce Failed, got {result:?}"
        );
    }

    #[test]
    fn json_accepted_placeholder_content_fails_and_includes_raw() {
        let result = parse_role_response(r#"{"status":"accepted","content":"..."}"#);
        let RoleResult::Failed { reason } = result else {
            panic!("placeholder '...' content must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("placeholder"),
            "failure reason must mention 'placeholder'; got: {reason}"
        );
        assert!(
            reason.contains(r#"{"status":"accepted","content":"..."}"#),
            "failure reason must include the raw provider response; got: {reason}"
        );
    }

    #[test]
    fn json_rejected_empty_reason_fails() {
        let result = parse_role_response(r#"{"status":"rejected","reason":""}"#);
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "empty reason must produce Failed, got {result:?}"
        );
    }

    #[test]
    fn json_rejected_placeholder_reason_fails_and_includes_raw() {
        let result = parse_role_response(r#"{"status":"rejected","reason":"..."}"#);
        let RoleResult::Failed { reason } = result else {
            panic!("placeholder '...' reason must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("placeholder"),
            "failure reason must mention 'placeholder'; got: {reason}"
        );
        assert!(
            reason.contains("..."),
            "failure reason must include the '...' placeholder text so telemetry is not elided; got: {reason}"
        );
    }

    #[test]
    fn json_unknown_status_fails() {
        let result = parse_role_response(r#"{"status":"pending","content":"draft"}"#);
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "unknown status must produce Failed, got {result:?}"
        );
    }

    #[test]
    fn malformed_json_fails() {
        let result = parse_role_response("not json at all");
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "malformed JSON must produce Failed, got {result:?}"
        );
    }

    #[test]
    fn fenced_json_parses() {
        let input = "```json\n{\"status\":\"accepted\",\"content\":\"draft\"}\n```";
        let result = parse_role_response(input);
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "draft"),
            "fenced JSON must parse to Accepted {{ 'draft' }}, got {result:?}"
        );
    }

    #[test]
    fn preamble_then_json_parses_if_object_extractable() {
        let input = "Here is the result:\n{\"status\":\"accepted\",\"content\":\"draft\"}";
        let result = parse_role_response(input);
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "draft"),
            "JSON after preamble must parse to Accepted {{ 'draft' }}, got {result:?}"
        );
    }

    #[test]
    fn json_with_trailing_text_parses_first_object() {
        let input = r#"{"status":"accepted","content":"draft"}\nSome trailing explanation the model added."#;
        let result = parse_role_response(input);
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "draft"),
            "trailing text after JSON must be ignored, got {result:?}"
        );
    }

    #[test]
    fn json_with_braces_inside_string_parses() {
        let input = r#"{"status":"accepted","content":"use {} in templates"}"#;
        let result = parse_role_response(input);
        assert!(
            matches!(result, RoleResult::Accepted { ref content } if content == "use {} in templates"),
            "braces inside string must not affect depth count, got {result:?}"
        );
    }

    #[test]
    fn unbalanced_json_object_fails() {
        let result = parse_role_response(r#"{"status":"accepted","content":"oops""#);
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "unbalanced JSON must produce Failed, got {result:?}"
        );
    }

    // --- handler-level tests (provider → RoleResult) ---

    #[test]
    fn provider_error_still_maps_to_failed() {
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
        assert!(
            matches!(
                event,
                DeliberationEvent::RoleReturned {
                    result: RoleResult::Failed { .. },
                    ..
                }
            ),
            "provider error must map to Failed, not Rejected, got {event:?}"
        );
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
        assert!(
            prompt.contains("\"status\""),
            "expected prompt to include JSON schema instructions, got: {prompt}"
        );
    }

    // --- run_machine integration tests ---

    #[test]
    fn run_machine_with_provider_handler_success() {
        let machine = ProvidedMachine {
            handler: ProviderBackedDeliberationHandler::new(ScriptedProvider::from_strs(&[
                r#"{"status":"accepted","content":"draft"}"#,
                r#"{"status":"accepted","content":"review"}"#,
                r#"{"status":"accepted","content":"approved"}"#,
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
                r#"{"status":"accepted","content":"draft v1"}"#,
                r#"{"status":"accepted","content":"review"}"#,
                r#"{"status":"rejected","reason":"needs changes"}"#,
                r#"{"status":"accepted","content":"draft v2"}"#,
                r#"{"status":"accepted","content":"review ok"}"#,
                r#"{"status":"accepted","content":"approved"}"#,
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
}
