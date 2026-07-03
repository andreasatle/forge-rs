//! Ollama provider — calls the local Ollama `/api/generate` endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::client::ProviderClient;
use crate::providers::http_error::HttpProviderErrorClassifier;
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
                    kind: HttpProviderErrorClassifier::classify_status(status),
                    message: format!("HTTP {status}"),
                },
                ureq::Error::Transport(transport_err) => ProviderError {
                    kind: HttpProviderErrorClassifier::classify_transport(&transport_err),
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
        let cases = [
            (r#"{"response":"hello"}"#, "hello", None),
            (
                r#"{"response":"hello","done_reason":"stop"}"#,
                "hello",
                Some("stop"),
            ),
        ];
        for (json, expected_content, expected_finish_reason) in cases {
            let parsed: GenerateResponse = serde_json::from_str(json).unwrap();
            let result = map_generate_response(parsed).unwrap();
            assert_eq!(result.content, expected_content);
            assert_eq!(result.finish_reason.as_deref(), expected_finish_reason);
        }
    }

    #[test]
    fn ollama_response_missing_response_is_terminal() {
        let json = r#"{"other_field":"value"}"#;
        let parsed: GenerateResponse = serde_json::from_str(json).unwrap();
        let err = map_generate_response(parsed).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
    }

    #[test]
    fn ollama_classify_status() {
        let cases = [
            (429, ProviderErrorKind::Retryable),
            (500, ProviderErrorKind::Retryable),
            (503, ProviderErrorKind::Retryable),
            (404, ProviderErrorKind::Terminal),
            (400, ProviderErrorKind::Terminal),
        ];
        for (status, expected) in cases {
            assert_eq!(
                HttpProviderErrorClassifier::classify_status(status),
                expected,
                "status {status}"
            );
        }
    }

    #[test]
    fn is_timeout_source_classification() {
        let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let timed_out_boxed: Box<dyn std::error::Error + 'static> = Box::new(timed_out);
        let refused_boxed: Box<dyn std::error::Error + 'static> = Box::new(refused);

        assert!(HttpProviderErrorClassifier::is_timeout_source(Some(
            timed_out_boxed.as_ref()
        )));
        assert!(!HttpProviderErrorClassifier::is_timeout_source(Some(
            refused_boxed.as_ref()
        )));
        assert!(!HttpProviderErrorClassifier::is_timeout_source(None));
    }

    #[test]
    fn ollama_provider_format_serialization() {
        let cases = [(Some("json".to_string()), true), (None, false)];
        for (format, expect_present) in cases {
            let req = GenerateRequest {
                model: "test".to_string(),
                prompt: "hello".to_string(),
                stream: false,
                options: GenerateOptions { num_predict: 512 },
                format,
            };
            let json = serde_json::to_string(&req).unwrap();
            assert_eq!(
                json.contains("\"format\":\"json\""),
                expect_present,
                "got: {json}"
            );
        }
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
}
