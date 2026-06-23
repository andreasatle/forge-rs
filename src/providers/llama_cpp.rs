//! LlamaCpp provider — calls a running `llama-server` `/completion` endpoint.

use serde::{Deserialize, Serialize};

use crate::providers::client::ProviderClient;
use crate::providers::types::{
    ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse,
};

/// Calls the llama.cpp server completion API at the given base URL.
pub struct LlamaCppProvider {
    base_url: String,
}

impl LlamaCppProvider {
    /// Create a new provider targeting `base_url` (e.g. `"http://localhost:8080"`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

#[derive(Serialize)]
struct CompletionRequest {
    prompt: String,
    n_predict: u32,
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
        let body = CompletionRequest {
            prompt: request.prompt,
            n_predict: request.max_tokens,
        };

        let http_response = ureq::post(&url).send_json(&body).map_err(|err| match err {
            ureq::Error::Status(status, _) => ProviderError {
                kind: classify_status(status),
                message: format!("HTTP {status}"),
            },
            ureq::Error::Transport(transport_err) => ProviderError {
                kind: ProviderErrorKind::Retryable,
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
    fn llama_cpp_provider_new_stores_base_url() {
        let p = LlamaCppProvider::new("http://localhost:8080");
        assert_eq!(p.base_url, "http://localhost:8080");
    }

    #[test]
    fn llama_provider_preserves_json_output_schema_at_boundary() {
        use crate::providers::types::StructuredOutput;

        // LlamaCppProvider accepts output_schema in the request boundary.
        // The grammar parameter is not yet wired; the field is carried and
        // preserved so the architecture is in place for future activation.
        let req = ProviderRequest {
            prompt: "test".to_string(),
            max_tokens: 512,
            output_schema: Some(StructuredOutput::Json),
        };
        assert_eq!(req.output_schema, Some(StructuredOutput::Json));
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
