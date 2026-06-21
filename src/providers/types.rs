//! Typed request/response/error structures for the provider boundary.

/// A request sent to a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    /// The prompt text to send.
    pub prompt: String,
}

/// A successful response from a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponse {
    /// The generated content.
    pub content: String,
}

/// Whether a provider error can be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Transient failure; a retry may succeed.
    Retryable,
    /// Permanent failure; retrying will not help.
    Terminal,
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
        };
        assert_eq!(req.prompt, "hello");
    }

    #[test]
    fn response_roundtrip() {
        let resp = ProviderResponse {
            content: "world".to_string(),
        };
        assert_eq!(resp.content, "world");
    }

    #[test]
    fn error_kinds_are_distinct() {
        assert_ne!(ProviderErrorKind::Retryable, ProviderErrorKind::Terminal);
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
