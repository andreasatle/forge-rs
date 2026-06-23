//! Ollama provider — calls the local Ollama `/api/generate` endpoint.

use serde::{Deserialize, Serialize};

use crate::providers::client::ProviderClient;
use crate::providers::types::{
    ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse, StructuredOutput,
};

/// Calls the Ollama generate API at the given base URL.
pub struct OllamaProvider {
    base_url: String,
    model: String,
}

impl OllamaProvider {
    /// Create a new provider targeting `base_url` (e.g. `"http://localhost:11434"`) with `model`.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
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
            format: request
                .output_schema
                .map(|StructuredOutput::Json| "json".to_string()),
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
    fn ollama_provider_new_stores_fields() {
        let p = OllamaProvider::new("http://localhost:11434", "llama3");
        assert_eq!(p.base_url, "http://localhost:11434");
        assert_eq!(p.model, "llama3");
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
