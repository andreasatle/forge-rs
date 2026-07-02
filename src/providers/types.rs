//! Typed request/response/error structures for the provider boundary.

/// The kind of structured output the provider should produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuredOutput {
    /// Request generic JSON-formatted output, with no schema constraint.
    Json,
    /// Request output constrained by the given GBNF grammar.
    ///
    /// Providers that do not support grammar-constrained decoding (e.g.
    /// Ollama) fall back to generic JSON mode.
    Grammar(String),
}

/// A request sent to a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    /// The prompt text to send.
    pub prompt: String,
    /// Maximum number of tokens to generate.
    pub max_tokens: u32,
    /// Optional structured-output hint for the provider.
    ///
    /// Providers may use this to activate native JSON mode or a grammar
    /// constraint. Providers that do not support structured output must
    /// carry the field unchanged and ignore it.
    pub output_schema: Option<StructuredOutput>,
}

/// A successful response from a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponse {
    /// The generated content.
    pub content: String,
    /// Why generation stopped, if the provider exposes it.
    pub finish_reason: Option<String>,
}

/// Whether a provider error can be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Transient failure; a retry may succeed.
    Retryable,
    /// Permanent failure; retrying will not help.
    Terminal,
    /// The provider did not respond within the configured deadline.
    Timeout,
}

/// An error returned by a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    /// Classification of the failure.
    pub kind: ProviderErrorKind,
    /// Human-readable description.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = ProviderRequest {
            prompt: "hello".to_string(),
            max_tokens: 512,
            output_schema: None,
        };
        assert_eq!(req.prompt, "hello");
        assert_eq!(req.max_tokens, 512);
        assert_eq!(req.output_schema, None);
    }

    #[test]
    fn provider_request_clone_preserves_output_schema() {
        let req = ProviderRequest {
            prompt: "test".to_string(),
            max_tokens: 256,
            output_schema: Some(StructuredOutput::Json),
        };
        let cloned = req.clone();
        assert_eq!(cloned.output_schema, Some(StructuredOutput::Json));
        assert_eq!(cloned.prompt, req.prompt);
        assert_eq!(cloned.max_tokens, req.max_tokens);
    }

    #[test]
    fn response_roundtrip() {
        let resp = ProviderResponse {
            content: "world".to_string(),
            finish_reason: Some("stop".to_string()),
        };
        assert_eq!(resp.content, "world");
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn response_finish_reason_optional() {
        let resp = ProviderResponse {
            content: "world".to_string(),
            finish_reason: None,
        };
        assert!(resp.finish_reason.is_none());
    }

    #[test]
    fn error_kinds_are_distinct() {
        assert_ne!(ProviderErrorKind::Retryable, ProviderErrorKind::Terminal);
    }

    #[test]
    fn provider_error_timeout_kind_exists() {
        let err = ProviderError {
            kind: ProviderErrorKind::Timeout,
            message: "deadline exceeded".to_string(),
        };
        assert_eq!(err.kind, ProviderErrorKind::Timeout);
        assert_ne!(err.kind, ProviderErrorKind::Retryable);
        assert_ne!(err.kind, ProviderErrorKind::Terminal);
    }

    #[test]
    fn error_carries_message() {
        let err = ProviderError {
            kind: ProviderErrorKind::Retryable,
            message: "rate limited".to_string(),
        };
        assert_eq!(err.message, "rate limited");
        assert_eq!(err.kind, ProviderErrorKind::Retryable);
    }

    #[test]
    fn terminal_error() {
        let err = ProviderError {
            kind: ProviderErrorKind::Terminal,
            message: "invalid api key".to_string(),
        };
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
    }
}
