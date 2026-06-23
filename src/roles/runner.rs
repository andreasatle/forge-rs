//! Provider-backed role execution.
//!
//! `RoleRunner` owns one complete role round-trip: render prompt, call provider,
//! parse JSON, retry on protocol failure. The deliberation layer above sees only
//! `RoleRequest` in and `RoleResult` out.

use serde::Deserialize;

use crate::artifacts::{ArtifactUpdate, ArtifactView};
use crate::machines::deliberation::event::RoleResult;
use crate::machines::deliberation::state::{DeliberationRole, RevisionFeedback};
use crate::providers::{ProviderClient, ProviderRequest};
use crate::telemetry::{TelemetryEvent, TelemetryRecord, TelemetrySink};
use crate::tools::{FileToolExecutor, FileToolResponse, parse_tool_request};

/// A read-only view of the artifact made available to role tool loops.
#[derive(Debug)]
pub struct RoleToolContext {
    /// The artifact snapshot the role may read from and accumulate changes against.
    pub artifact_view: ArtifactView,
}

/// All inputs needed to execute one role invocation.
#[derive(Debug)]
pub struct RoleRequest {
    /// The role to invoke.
    pub role: DeliberationRole,
    /// The objective to pass to the role.
    pub objective: String,
    /// Content produced by the Producer. `None` when dispatching Producer.
    pub producer_content: Option<String>,
    /// Content produced by the Critic. `None` when dispatching Producer or Critic.
    pub critic_content: Option<String>,
    /// Accumulated Referee rejection feedback. Empty on the first pass.
    pub feedback: Vec<RevisionFeedback>,
    /// File tool context. When `Some`, the role may issue tool requests before
    /// returning a final result. When `None`, tool request JSON is still detected
    /// but produces an error observation rather than a real tool execution.
    pub tool_context: Option<RoleToolContext>,
}

/// The output of a completed role invocation.
pub struct RoleRunOutput {
    /// The semantic result returned by the role.
    pub result: RoleResult,
    /// Pending file changes accumulated by tool calls during the role loop.
    ///
    /// `None` when no tool calls were made or when no artifact view was
    /// provided. Non-empty changes are returned even on protocol failure so
    /// that callers can decide what to do with partial work.
    pub artifact_update: Option<ArtifactUpdate>,
}

/// Execute one role invocation end-to-end and return its result.
pub trait RoleRunner {
    /// Run the role described by `request` and record protocol telemetry.
    fn run_role(&self, request: RoleRequest, telemetry: &dyn TelemetrySink) -> RoleRunOutput;
}

/// Provider-backed implementation of [`RoleRunner`].
///
/// Wraps a [`ProviderClient`] and owns all role-layer logic: prompt rendering,
/// provider invocation, JSON extraction/parsing, and protocol retry.
pub struct ProviderRoleRunner<P> {
    provider: P,
}

impl<P> ProviderRoleRunner<P> {
    /// Wrap a provider in a new runner.
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

/// Maximum number of tokens to request per provider call.
const MAX_RESPONSE_TOKENS: u32 = 1024;

/// Maximum number of additional provider calls after the initial response has
/// failed protocol parsing or validation.
const MAX_PROTOCOL_RETRIES: usize = 2;

/// Maximum number of tool calls within a single role invocation before the
/// loop is declared a protocol failure.
const MAX_TOOL_STEPS: usize = 5;

impl<P: ProviderClient> RoleRunner for ProviderRoleRunner<P> {
    fn run_role(&self, request: RoleRequest, telemetry: &dyn TelemetrySink) -> RoleRunOutput {
        let subsource = role_subsource(&request.role);
        let has_tools = request.tool_context.is_some();

        let base_prompt = {
            let core = render_role_prompt(
                &request.role,
                &request.objective,
                request.producer_content.as_deref(),
                request.critic_content.as_deref(),
                &request.feedback,
            );
            if has_tools {
                format!("{core}\n\n{}", render_tool_section())
            } else {
                core
            }
        };

        let mut executor: Option<FileToolExecutor> = request
            .tool_context
            .map(|ctx| FileToolExecutor::new(ctx.artifact_view));

        let mut current_prompt = base_prompt.clone();
        let mut protocol_attempt: usize = 1;
        let mut tool_steps: usize = 0;

        loop {
            telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                subsource,
                TelemetryEvent::RolePromptRendered {
                    prompt: current_prompt.clone(),
                    attempt_count: protocol_attempt,
                },
            ));

            let response = match self.provider.call(ProviderRequest {
                prompt: current_prompt.clone(),
                max_tokens: MAX_RESPONSE_TOKENS,
            }) {
                Ok(r) => r,
                Err(err) => {
                    return RoleRunOutput {
                        result: RoleResult::Failed {
                            reason: format!("provider error ({:?}): {}", err.kind, err.message),
                        },
                        artifact_update: extract_update(&mut executor),
                    };
                }
            };

            telemetry.record(TelemetryRecord::new_with_subsource(
                "RoleMachine",
                subsource,
                TelemetryEvent::ProviderResponseReceived {
                    raw_response: response.content.clone(),
                    attempt_count: protocol_attempt,
                },
            ));

            // Check for a tool request before trying to parse as a role result.
            let trimmed = strip_code_fence(response.content.trim());
            if let Some(json_str) = extract_json_object(trimmed)
                && let Ok(tool_req) = parse_tool_request(json_str)
            {
                tool_steps += 1;
                let tool_name = tool_name_of(&tool_req);
                telemetry.record(TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    subsource,
                    TelemetryEvent::ToolRequested {
                        tool: tool_name.clone(),
                    },
                ));

                if tool_steps > MAX_TOOL_STEPS {
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ToolLoopLimitReached,
                    ));
                    return RoleRunOutput {
                        result: RoleResult::Failed {
                            reason: "tool loop limit reached".to_string(),
                        },
                        artifact_update: extract_update(&mut executor),
                    };
                }

                let observation = match &mut executor {
                    Some(exec) => format_tool_observation(&exec.execute(tool_req)),
                    None => r#"{"ok":false,"error":"no file tools available"}"#.to_string(),
                };

                telemetry.record(TelemetryRecord::new_with_subsource(
                    "RoleMachine",
                    subsource,
                    TelemetryEvent::ToolReturned {
                        tool: tool_name,
                        result: observation.clone(),
                    },
                ));

                current_prompt = format!("{current_prompt}\n\nTool result:\n{observation}");
                protocol_attempt = 1;
                continue;
            }

            // Not a tool request — try to parse as a role result.
            match try_parse_role_response(&response.content) {
                Ok(result) => {
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ParseSucceeded {
                            attempt_count: protocol_attempt,
                        },
                    ));
                    return RoleRunOutput {
                        result,
                        artifact_update: extract_update(&mut executor),
                    };
                }
                Err(parse_error) => {
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ParseFailed {
                            raw_response: response.content.clone(),
                            parse_error: parse_error.clone(),
                            attempt_count: protocol_attempt,
                        },
                    ));
                    if protocol_attempt > MAX_PROTOCOL_RETRIES {
                        return RoleRunOutput {
                            result: RoleResult::Failed {
                                reason: parse_error,
                            },
                            artifact_update: extract_update(&mut executor),
                        };
                    }
                    let next_attempt = protocol_attempt + 1;
                    telemetry.record(TelemetryRecord::new_with_subsource(
                        "RoleMachine",
                        subsource,
                        TelemetryEvent::ProtocolRetry {
                            parse_error: parse_error.clone(),
                            attempt_count: next_attempt,
                        },
                    ));
                    current_prompt = render_retry_prompt(&base_prompt, &parse_error);
                    protocol_attempt = next_attempt;
                }
            }
        }
    }
}

/// Consumes the executor and returns its pending update, or `None` when empty.
fn extract_update(executor: &mut Option<FileToolExecutor>) -> Option<ArtifactUpdate> {
    executor.take().and_then(|e| {
        let update = e.into_update();
        if update.changes.is_empty() {
            None
        } else {
            Some(update)
        }
    })
}

/// Returns a short name for a tool request (used in telemetry).
fn tool_name_of(req: &crate::tools::FileToolRequest) -> String {
    use crate::tools::FileToolRequest;
    match req {
        FileToolRequest::ListFiles => "list_files",
        FileToolRequest::ReadFile { .. } => "read_file",
        FileToolRequest::WriteFile { .. } => "write_file",
        FileToolRequest::ReplaceText { .. } => "replace_text",
        FileToolRequest::DeleteFile { .. } => "delete_file",
    }
    .to_string()
}

/// Serialises a [`FileToolResponse`] as a compact JSON observation string.
fn format_tool_observation(response: &FileToolResponse) -> String {
    match response {
        FileToolResponse::FileList { paths } => {
            let files: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            serde_json::to_string(&serde_json::json!({"ok": true, "files": files}))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
        }
        FileToolResponse::FileContents { content, .. } => {
            const MAX_CONTENT: usize = 4000;
            let truncated = &content[..content.len().min(MAX_CONTENT)];
            serde_json::to_string(&serde_json::json!({"ok": true, "content": truncated}))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
        }
        FileToolResponse::UpdateRecorded { description } => {
            serde_json::to_string(&serde_json::json!({"ok": true, "description": description}))
                .unwrap_or_else(|_| r#"{"ok":true}"#.to_string())
        }
        FileToolResponse::Failed { reason } => {
            serde_json::to_string(&serde_json::json!({"ok": false, "error": reason}))
                .unwrap_or_else(|_| r#"{"ok":false}"#.to_string())
        }
    }
}

/// Returns the tool-availability section appended to a prompt when tools are enabled.
fn render_tool_section() -> &'static str {
    "Available file tools:\n\
     {\"tool\":\"list_files\"}\n\
     {\"tool\":\"read_file\",\"path\":\"README.md\"}\n\
     {\"tool\":\"write_file\",\"path\":\"output.txt\",\"content\":\"...\"}\n\
     {\"tool\":\"replace_text\",\"path\":\"output.txt\",\"old\":\"...\",\"new\":\"...\"}\n\
     {\"tool\":\"delete_file\",\"path\":\"old.txt\"}\n\
     You may return either:\n\
     1. a tool request JSON, or\n\
     2. a final role result JSON.\n\
     Return exactly one JSON object."
}

fn role_subsource(role: &DeliberationRole) -> &'static str {
    match role {
        DeliberationRole::Producer => "Producer",
        DeliberationRole::Critic => "Critic",
        DeliberationRole::Referee => "Referee",
    }
}

fn render_retry_prompt(original_prompt: &str, parse_error: &str) -> String {
    format!(
        "{original_prompt}\n\n\
         Your previous response could not be parsed: {parse_error}\n\
         Return only one JSON object matching one of these schemas:\n\
         {{\"status\":\"accepted\",\"content\":\"...\"}}\n\
         {{\"status\":\"rejected\",\"reason\":\"...\"}}"
    )
}

/// Build a prompt for a single role invocation.
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

/// Internal serde type for JSON role responses from the provider.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum JsonRoleResponse {
    Accepted { content: String },
    Rejected { reason: String },
}

fn try_parse_role_response(raw_response: &str) -> Result<RoleResult, String> {
    let text = strip_code_fence(raw_response.trim());
    let json_str = match extract_json_object(text) {
        Some(s) => s,
        None => {
            return Err("no JSON object found in role response".to_string());
        }
    };
    let result = match serde_json::from_str::<JsonRoleResponse>(json_str) {
        Ok(JsonRoleResponse::Accepted { content }) => {
            if content.trim().is_empty() {
                return Err("accepted response has empty content".to_string());
            } else if content.trim() == "..." {
                return Err("role response has placeholder accepted content".to_string());
            } else {
                RoleResult::Accepted { content }
            }
        }
        Ok(JsonRoleResponse::Rejected { reason }) => {
            if reason.trim().is_empty() || reason.trim() == "..." {
                return Err("role response has placeholder reason".to_string());
            } else {
                RoleResult::Rejected { reason }
            }
        }
        Err(err) => return Err(format!("JSON parse error: {err}")),
    };
    Ok(result)
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
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => {
                i += 2;
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
fn parse_role_response(raw_response: &str) -> RoleResult {
    try_parse_role_response(raw_response).unwrap_or_else(|reason| RoleResult::Failed { reason })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifacts::{ArtifactView, FileChange};
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
        requests: RefCell<Vec<ProviderRequest>>,
    }

    impl ScriptedProvider {
        fn from_strs(responses: &[&str]) -> Self {
            Self {
                responses: RefCell::new(responses.iter().map(|s| s.to_string()).collect()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProviderClient for ScriptedProvider {
        fn call(&self, req: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            self.requests.borrow_mut().push(req);
            let content = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("ScriptedProvider: responses exhausted");
            Ok(ProviderResponse {
                content,
                finish_reason: None,
            })
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
    fn json_accepted_placeholder_content_fails_without_including_raw() {
        let result = parse_role_response(r#"{"status":"accepted","content":"..."}"#);
        let RoleResult::Failed { reason } = result else {
            panic!("placeholder '...' content must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("placeholder"),
            "failure reason must mention 'placeholder'; got: {reason}"
        );
        assert!(!reason.contains("raw:"));
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
    fn json_rejected_placeholder_reason_fails_without_including_raw() {
        let result = parse_role_response(r#"{"status":"rejected","reason":"..."}"#);
        let RoleResult::Failed { reason } = result else {
            panic!("placeholder '...' reason must produce Failed, got {result:?}");
        };
        assert!(
            reason.contains("placeholder"),
            "failure reason must mention 'placeholder'; got: {reason}"
        );
        assert!(!reason.contains("raw:"));
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

    // --- ProviderRoleRunner tests ---

    #[test]
    fn provider_error_still_maps_to_failed() {
        let runner = ProviderRoleRunner::new(FailingProvider {
            kind: ProviderErrorKind::Retryable,
            message: "rate limited".to_string(),
        });
        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "write a poem".to_string(),
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "provider error must map to Failed, not Rejected, got {result:?}"
        );
    }

    #[test]
    fn provider_terminal_error_maps_to_failed() {
        let runner = ProviderRoleRunner::new(FailingProvider {
            kind: ProviderErrorKind::Terminal,
            message: "auth error".to_string(),
        });
        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "write a poem".to_string(),
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;
        assert!(
            matches!(result, RoleResult::Failed { .. }),
            "terminal provider error must map to Failed, not {result:?}"
        );
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

    #[test]
    fn provider_role_runner_retries_malformed_json() {
        let provider = ScriptedProvider::from_strs(&[
            "invalid text",
            r#"{"status":"accepted","content":"recovered"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "recover output".to_string(),
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;

        assert!(matches!(result, RoleResult::Accepted { ref content } if content == "recovered"));
        assert_eq!(provider.requests.borrow().len(), 2);
    }

    #[test]
    fn retry_prompt_contains_parse_error() {
        let provider = ScriptedProvider::from_strs(&[
            "invalid text",
            r#"{"status":"accepted","content":"recovered"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "recover output".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        let retry_prompt = &requests[1].prompt;
        assert!(retry_prompt.contains("no JSON object found"));
        assert!(!retry_prompt.contains("invalid text"));
        assert!(retry_prompt.contains("Objective: recover output"));
        assert!(retry_prompt.contains(r#"{"status":"accepted","content":"..."}"#));
    }

    #[test]
    fn retry_limit_returns_failure() {
        let provider =
            ScriptedProvider::from_strs(&["invalid one", "invalid two", "invalid three"]);
        let runner = ProviderRoleRunner::new(&provider);

        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Producer,
                    objective: "never valid".to_string(),
                    producer_content: None,
                    critic_content: None,
                    feedback: vec![],
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;

        assert!(matches!(result, RoleResult::Failed { .. }));
        assert_eq!(provider.requests.borrow().len(), 3);
    }

    #[test]
    fn provider_role_runner_returns_semantic_rejection_without_retry() {
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"rejected","reason":"needs revision"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        let result = runner
            .run_role(
                RoleRequest {
                    role: DeliberationRole::Referee,
                    objective: "review output".to_string(),
                    producer_content: Some("draft".to_string()),
                    critic_content: Some("review".to_string()),
                    feedback: vec![],
                    tool_context: None,
                },
                &crate::telemetry::NoopTelemetry,
            )
            .result;

        assert!(
            matches!(result, RoleResult::Rejected { ref reason } if reason == "needs revision"),
            "semantic rejection must not retry, got {result:?}"
        );
        assert_eq!(provider.requests.borrow().len(), 1);
    }

    #[test]
    fn protocol_retry_records_role_layer_telemetry() {
        use crate::telemetry::{TelemetryEvent, VecTelemetry};

        let provider = ScriptedProvider::from_strs(&[
            "invalid text",
            r#"{"status":"accepted","content":"recovered"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);
        let telemetry = VecTelemetry::new();

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "recover output".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: None,
            },
            &telemetry,
        );

        let records = telemetry.records();
        assert!(records.iter().all(|record| record.source == "RoleMachine"));
        assert!(records.iter().any(|record| matches!(
            &record.event,
            TelemetryEvent::RolePromptRendered {
                attempt_count: 2,
                prompt,
            } if prompt.contains("no JSON object found")
        )));
        assert!(records.iter().any(|record| matches!(
            &record.event,
            TelemetryEvent::ProviderResponseReceived {
                attempt_count: 1,
                raw_response,
            } if raw_response == "invalid text"
        )));
        assert!(records.iter().any(|record| matches!(
            &record.event,
            TelemetryEvent::ParseFailed { parse_error, .. }
                if parse_error.contains("no JSON object found")
        )));
        assert!(records.iter().any(|record| matches!(
            &record.event,
            TelemetryEvent::ProtocolRetry {
                attempt_count: 2,
                ..
            }
        )));
        assert!(records.iter().any(|record| matches!(
            record.event,
            TelemetryEvent::ParseSucceeded { attempt_count: 2 }
        )));
    }

    #[test]
    fn role_events_use_role_machine_source() {
        use crate::telemetry::FileTelemetry;

        let dir = std::env::temp_dir().join("forge-role-machine-source-test");
        let _ = std::fs::remove_dir_all(&dir);
        let telemetry = FileTelemetry::new(dir.clone()).unwrap();
        let provider = ScriptedProvider::from_strs(&[
            "invalid text",
            r#"{"status":"accepted","content":"recovered"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "recover output".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: None,
            },
            &telemetry,
        );

        assert!(
            dir.join("000001--role-machine--producer--role-prompt-rendered.txt")
                .exists()
        );
        assert!(
            dir.join("000003--role-machine--producer--parse-failed.txt")
                .exists()
        );
        assert!(
            dir.join("000004--role-machine--producer--protocol-retry.txt")
                .exists()
        );
    }

    // --- git helpers for tool tests that need a real ArtifactView ---

    static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-runner-tools-{label}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &PathBuf, args: &[&str]) {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git failed");
    }

    fn git_rev(dir: &PathBuf) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("git rev-parse failed");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn make_view(label: &str) -> (TempDir, ArtifactView) {
        let temp = TempDir::new(label);
        let seed = temp.0.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(seed.join("hello.txt"), "hello world\n").unwrap();
        git(&seed, &["add", "hello.txt"]);
        git(&seed, &["commit", "--quiet", "-m", "init"]);
        let bare = temp.0.join("bare.git");
        Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .status()
            .expect("git clone --bare failed");
        let sha = git_rev(&bare);
        (
            temp,
            ArtifactView {
                repo_path: bare,
                commit_sha: sha,
            },
        )
    }

    fn dummy_view() -> ArtifactView {
        ArtifactView {
            repo_path: PathBuf::from("/nonexistent"),
            commit_sha: "deadbeef".to_string(),
        }
    }

    // --- tool loop tests ---

    #[test]
    fn role_runner_executes_read_file_tool_then_accepts() {
        let (_temp, view) = make_view("read-file-tool");
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"read_file","path":"hello.txt"}"#,
            r#"{"status":"accepted","content":"read the file"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "read hello.txt".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: Some(RoleToolContext {
                    artifact_view: view,
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == "read the file"),
            "expected Accepted after read_file tool loop, got {:?}",
            output.result
        );
        assert_eq!(
            provider.requests.borrow().len(),
            2,
            "must call provider twice"
        );
        let second_prompt = &provider.requests.borrow()[1].prompt;
        assert!(
            second_prompt.contains("Tool result:"),
            "second prompt must include tool observation"
        );
        assert!(
            second_prompt.contains("hello world"),
            "observation must include file content"
        );
    }

    #[test]
    fn role_runner_records_write_file_tool_update() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"write_file","path":"output.txt","content":"hello"}"#,
            r#"{"status":"accepted","content":"wrote the file"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "write a file".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: Some(RoleToolContext {
                    artifact_view: dummy_view(),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { .. }),
            "expected Accepted, got {:?}",
            output.result
        );
        let update = output
            .artifact_update
            .expect("write_file must produce an artifact_update");
        assert_eq!(update.changes.len(), 1);
        match &update.changes[0] {
            FileChange::Write { path, content } => {
                assert_eq!(path, "output.txt");
                assert_eq!(content, "hello");
            }
            other => panic!("expected Write change, got {other:?}"),
        }
    }

    #[test]
    fn role_runner_rejects_tool_when_no_artifact_view() {
        let provider = ScriptedProvider::from_strs(&[
            r#"{"tool":"list_files"}"#,
            r#"{"status":"accepted","content":"used no tools"}"#,
        ]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "do the thing".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == "used no tools"),
            "tool request without view must produce error observation and allow final result; got {:?}",
            output.result
        );
        assert_eq!(provider.requests.borrow().len(), 2);
        let second_prompt = &provider.requests.borrow()[1].prompt;
        assert!(
            second_prompt.contains("no file tools available"),
            "second prompt must include error observation"
        );
    }

    #[test]
    fn role_runner_stops_at_tool_loop_limit() {
        // Each call returns a write_file request; the 6th triggers the limit.
        let responses: Vec<&str> =
            vec![r#"{"tool":"write_file","path":"f.txt","content":"x"}"#; MAX_TOOL_STEPS + 1];
        let provider = ScriptedProvider::from_strs(&responses);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "loop forever".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: Some(RoleToolContext {
                    artifact_view: dummy_view(),
                }),
            },
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Failed { ref reason } if reason.contains("tool loop limit")),
            "must fail after tool loop limit; got {:?}",
            output.result
        );
        assert_eq!(
            provider.requests.borrow().len(),
            MAX_TOOL_STEPS + 1,
            "provider must be called exactly MAX_TOOL_STEPS + 1 times"
        );
    }

    #[test]
    fn role_runner_uses_provider_response_content() {
        let provider =
            ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"the result"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        let output = runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "produce something".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

        assert!(
            matches!(output.result, RoleResult::Accepted { ref content } if content == "the result"),
            "role runner must use response.content; got {:?}",
            output.result
        );
    }

    #[test]
    fn scripted_provider_supports_request_response_objects() {
        let provider = ScriptedProvider::from_strs(&[r#"{"status":"accepted","content":"done"}"#]);
        let runner = ProviderRoleRunner::new(&provider);

        runner.run_role(
            RoleRequest {
                role: DeliberationRole::Producer,
                objective: "anything".to_string(),
                producer_content: None,
                critic_content: None,
                feedback: vec![],
                tool_context: None,
            },
            &crate::telemetry::NoopTelemetry,
        );

        let requests = provider.requests.borrow();
        assert_eq!(requests.len(), 1);
        assert!(
            !requests[0].prompt.is_empty(),
            "request must carry a prompt"
        );
        assert_eq!(
            requests[0].max_tokens, MAX_RESPONSE_TOKENS,
            "request must carry the runner's max_tokens constant"
        );
    }
}
