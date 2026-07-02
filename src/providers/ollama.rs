//! Ollama provider — calls the local Ollama `/api/generate` endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::client::ProviderClient;
use crate::providers::types::{
    ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse, StructuredOutput,
};

/// Calls the Ollama generate API at the given base URL.
pub struct OllamaProvider {
    base_url: String,
    model: String,
    agent: ureq::Agent,
}

impl OllamaProvider {
    /// Create a new provider targeting `base_url` with `model` and a per-request `timeout_secs` deadline.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, timeout_secs: u64) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(timeout_secs))
            .build();
        Self {
            base_url: base_url.into(),
            model: model.into(),
            agent,
        }
    }
}

#[derive(Serialize)]
struct GenerateOptions {
    num_predict: u32,
}

#[derive(Serialize)]
struct GenerateRequest {
    model: String,
    prompt: String,
    stream: bool,
    options: GenerateOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: Option<String>,
    done_reason: Option<String>,
}

fn classify_status(status: u16) -> ProviderErrorKind {
    match status {
        429 | 500..=599 => ProviderErrorKind::Retryable,
        _ => ProviderErrorKind::Terminal,
    }
}

fn is_timeout_source(source: Option<&(dyn std::error::Error + 'static)>) -> bool {
    source
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .map(|e| e.kind() == std::io::ErrorKind::TimedOut)
        .unwrap_or(false)
}

fn classify_transport(err: &ureq::Transport) -> ProviderErrorKind {
    if is_timeout_source(std::error::Error::source(err)) {
        ProviderErrorKind::Timeout
    } else {
        ProviderErrorKind::Retryable
    }
}

/// Resolve the Ollama `format` value for a structured-output request.
///
/// Ollama has no GBNF support, so a grammar request falls back to generic
/// JSON mode rather than being dropped.
fn resolve_format(output_schema: Option<StructuredOutput>) -> Option<String> {
    output_schema.map(|schema| match schema {
        StructuredOutput::Json | StructuredOutput::Grammar(_) => "json".to_string(),
    })
}

fn map_generate_response(r: GenerateResponse) -> Result<ProviderResponse, ProviderError> {
    match r.response {
        Some(content) => Ok(ProviderResponse {
            content,
            finish_reason: r.done_reason,
        }),
        None => Err(ProviderError {
            kind: ProviderErrorKind::Terminal,
            message: "response field missing from Ollama reply".to_string(),
        }),
    }
}

impl ProviderClient for OllamaProvider {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let url = format!("{}/api/generate", self.base_url);
        let body = GenerateRequest {
            model: self.model.clone(),
            prompt: request.prompt,
            stream: false,
            options: GenerateOptions {
                num_predict: request.max_tokens,
            },
            format: resolve_format(request.output_schema),
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

        let parsed: GenerateResponse = http_response.into_json().map_err(|err| ProviderError {
            kind: ProviderErrorKind::Terminal,
            message: format!("invalid JSON response: {err}"),
        })?;

        map_generate_response(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_response_parses_content() {
        let json = r#"{"response":"hello"}"#;
        let parsed: GenerateResponse = serde_json::from_str(json).unwrap();
        let result = map_generate_response(parsed).unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(result.finish_reason, None);
    }

    #[test]
    fn ollama_response_maps_done_reason_to_finish_reason() {
        let json = r#"{"response":"hello","done_reason":"stop"}"#;
        let parsed: GenerateResponse = serde_json::from_str(json).unwrap();
        let result = map_generate_response(parsed).unwrap();
        assert_eq!(result.content, "hello");
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn ollama_response_missing_response_is_terminal() {
        let json = r#"{"other_field":"value"}"#;
        let parsed: GenerateResponse = serde_json::from_str(json).unwrap();
        let err = map_generate_response(parsed).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
    }

    #[test]
    fn ollama_http_429_is_retryable() {
        assert_eq!(classify_status(429), ProviderErrorKind::Retryable);
    }

    #[test]
    fn ollama_http_500_is_retryable() {
        assert_eq!(classify_status(500), ProviderErrorKind::Retryable);
    }

    #[test]
    fn ollama_http_503_is_retryable() {
        assert_eq!(classify_status(503), ProviderErrorKind::Retryable);
    }

    #[test]
    fn ollama_http_404_is_terminal() {
        assert_eq!(classify_status(404), ProviderErrorKind::Terminal);
    }

    #[test]
    fn ollama_http_400_is_terminal() {
        assert_eq!(classify_status(400), ProviderErrorKind::Terminal);
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
    fn ollama_provider_uses_json_format_for_json_output() {
        let req = GenerateRequest {
            model: "test".to_string(),
            prompt: "hello".to_string(),
            stream: false,
            options: GenerateOptions { num_predict: 512 },
            format: Some("json".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"format\":\"json\""),
            "serialized body must include format=json; got: {json}"
        );
    }

    #[test]
    fn ollama_provider_falls_back_to_json_format_for_grammar_output() {
        // Ollama has no GBNF support; a grammar request must still produce
        // valid JSON via the generic json format, not be silently dropped.
        let format = resolve_format(Some(StructuredOutput::Grammar(
            "root ::= \"x\"".to_string(),
        )));
        assert_eq!(format.as_deref(), Some("json"));
    }

    #[test]
    fn ollama_provider_omits_format_when_no_output_schema() {
        let req = GenerateRequest {
            model: "test".to_string(),
            prompt: "hello".to_string(),
            stream: false,
            options: GenerateOptions { num_predict: 512 },
            format: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("format"),
            "serialized body must omit format when None; got: {json}"
        );
    }
}
