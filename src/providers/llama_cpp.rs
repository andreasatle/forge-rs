//! LlamaCpp provider — calls a running `llama-server` `/completion` endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::client::ProviderClient;
use crate::providers::types::{
    ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse, StructuredOutput,
};

/// GBNF grammar that constrains llama.cpp output to a valid JSON object.
///
/// Uses the object-root form because all role outputs are JSON objects.
/// Reference: <https://github.com/ggerganov/llama.cpp/blob/master/grammars/json.gbnf>
const JSON_GBNF: &str = r#"root   ::= object
value  ::= object | array | string | number | ("true" | "false" | "null") ws

object ::=
  "{" ws (
            string ":" ws value
    ("," ws string ":" ws value)*
  )? "}" ws

array  ::=
  "[" ws (
            value
    ("," ws value)*
  )? "]" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

number ::= ("-"? ([0-9] | [1-9] [0-9]*)) ("." [0-9]+)? (([eE] [-+]? [0-9]+))? ws

ws ::= ([ \t\n] ws)?"#;

/// Calls the llama.cpp server completion API at the given base URL.
pub struct LlamaCppProvider {
    base_url: String,
    agent: ureq::Agent,
}

impl LlamaCppProvider {
    /// Create a new provider targeting `base_url` with a per-request `timeout_secs` deadline.
    pub fn new(base_url: impl Into<String>, timeout_secs: u64) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(timeout_secs))
            .build();
        Self {
            base_url: base_url.into(),
            agent,
        }
    }
}

#[derive(Serialize)]
struct CompletionRequest {
    prompt: String,
    n_predict: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    grammar: Option<String>,
}

#[derive(Deserialize)]
struct CompletionResponse {
    content: Option<String>,
}

fn classify_status(status: u16) -> ProviderErrorKind {
    match status {
        429 | 500..=599 => ProviderErrorKind::Retryable,
        _ => ProviderErrorKind::Terminal,
    }
}

fn classify_transport(err: &ureq::Transport) -> ProviderErrorKind {
    if is_timeout_source(std::error::Error::source(err)) {
        ProviderErrorKind::Timeout
    } else {
        ProviderErrorKind::Retryable
    }
}

fn is_timeout_source(source: Option<&(dyn std::error::Error + 'static)>) -> bool {
    source
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .map(|e| e.kind() == std::io::ErrorKind::TimedOut)
        .unwrap_or(false)
}

/// Resolve the GBNF grammar string to send for a given structured-output
/// request, falling back to [`JSON_GBNF`] for generic JSON mode.
fn resolve_grammar(output_schema: Option<&StructuredOutput>) -> Option<String> {
    output_schema.map(|schema| match schema {
        StructuredOutput::Json => JSON_GBNF.to_string(),
        StructuredOutput::Grammar(grammar) => grammar.clone(),
    })
}

fn map_completion_response(r: CompletionResponse) -> Result<ProviderResponse, ProviderError> {
    match r.content {
        Some(content) => Ok(ProviderResponse {
            content,
            finish_reason: None,
        }),
        None => Err(ProviderError {
            kind: ProviderErrorKind::Terminal,
            message: "content field missing from llama-server reply".to_string(),
        }),
    }
}

impl ProviderClient for LlamaCppProvider {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let url = format!("{}/completion", self.base_url);
        let grammar = resolve_grammar(request.output_schema.as_ref());
        let body = CompletionRequest {
            prompt: request.prompt,
            n_predict: request.max_tokens,
            grammar,
        };

        let http_response = self
            .agent
            .post(&url)
            .send_json(&body)
            .map_err(|err| match err {
                ureq::Error::Status(status, _) => ProviderError {
                    kind: classify_status(status),
                    message: format!("HTTP {status}"),
                },
                ureq::Error::Transport(transport_err) => ProviderError {
                    kind: classify_transport(&transport_err),
                    message: format!("connection error: {transport_err}"),
                },
            })?;

        let parsed: CompletionResponse =
            http_response.into_json().map_err(|err| ProviderError {
                kind: ProviderErrorKind::Terminal,
                message: format!("invalid JSON response: {err}"),
            })?;

        map_completion_response(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cpp_response_parses_content() {
        let json = r#"{"content":"hello"}"#;
        let parsed: CompletionResponse = serde_json::from_str(json).unwrap();
        let result = map_completion_response(parsed).unwrap();
        assert_eq!(result.content, "hello");
    }

    #[test]
    fn llama_cpp_response_missing_content_is_terminal() {
        let json = r#"{"other_field":"value"}"#;
        let parsed: CompletionResponse = serde_json::from_str(json).unwrap();
        let err = map_completion_response(parsed).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
    }

    #[test]
    fn llama_cpp_http_429_is_retryable() {
        assert_eq!(classify_status(429), ProviderErrorKind::Retryable);
    }

    #[test]
    fn llama_cpp_http_500_is_retryable() {
        assert_eq!(classify_status(500), ProviderErrorKind::Retryable);
    }

    #[test]
    fn llama_cpp_http_503_is_retryable() {
        assert_eq!(classify_status(503), ProviderErrorKind::Retryable);
    }

    #[test]
    fn llama_cpp_http_404_is_terminal() {
        assert_eq!(classify_status(404), ProviderErrorKind::Terminal);
    }

    #[test]
    fn llama_cpp_http_400_is_terminal() {
        assert_eq!(classify_status(400), ProviderErrorKind::Terminal);
    }

    #[test]
    fn llama_provider_includes_json_grammar_for_json_output() {
        let req = ProviderRequest {
            prompt: "test".to_string(),
            max_tokens: 512,
            output_schema: Some(StructuredOutput::Json),
        };
        let grammar = resolve_grammar(req.output_schema.as_ref());
        assert!(grammar.is_some());
        let g = grammar.unwrap();
        assert!(g.contains("root"));
        assert!(g.contains("object"));
    }

    #[test]
    fn llama_provider_omits_grammar_without_json_output() {
        let req = ProviderRequest {
            prompt: "test".to_string(),
            max_tokens: 512,
            output_schema: None,
        };
        let grammar = resolve_grammar(req.output_schema.as_ref());
        assert!(grammar.is_none());
    }

    #[test]
    fn llama_provider_uses_role_grammar_for_grammar_output() {
        let req = ProviderRequest {
            prompt: "test".to_string(),
            max_tokens: 512,
            output_schema: Some(StructuredOutput::Grammar("root ::= \"x\"".to_string())),
        };
        let grammar = resolve_grammar(req.output_schema.as_ref());
        assert_eq!(grammar.as_deref(), Some("root ::= \"x\""));
    }

    #[test]
    fn completion_request_grammar_omitted_when_none() {
        let body = CompletionRequest {
            prompt: "test".to_string(),
            n_predict: 128,
            grammar: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("grammar"));
    }

    #[test]
    fn completion_request_grammar_present_when_some() {
        let body = CompletionRequest {
            prompt: "test".to_string(),
            n_predict: 128,
            grammar: Some(JSON_GBNF.to_string()),
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"grammar\""));
    }

    #[test]
    fn is_timeout_source_detects_timed_out_io_error() {
        let ioe = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let boxed: Box<dyn std::error::Error + 'static> = Box::new(ioe);
        assert!(is_timeout_source(Some(boxed.as_ref())));
    }

    #[test]
    fn is_timeout_source_returns_false_for_other_io_errors() {
        let ioe = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let boxed: Box<dyn std::error::Error + 'static> = Box::new(ioe);
        assert!(!is_timeout_source(Some(boxed.as_ref())));
    }

    #[test]
    fn is_timeout_source_returns_false_for_none() {
        assert!(!is_timeout_source(None));
    }

    #[test]
    fn llama_provider_maps_response_to_provider_response() {
        let json = r#"{"content":"hello world"}"#;
        let parsed: CompletionResponse = serde_json::from_str(json).unwrap();
        let result = map_completion_response(parsed).unwrap();
        assert_eq!(result.content, "hello world");
        assert_eq!(result.finish_reason, None);
    }
}
