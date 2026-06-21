//! The `ProviderClient` trait — the boundary between machines and LLM providers.

use crate::providers::types::{ProviderError, ProviderRequest, ProviderResponse};

/// Synchronous interface every concrete provider must implement.
pub trait ProviderClient {
    /// Send a request and return either a response or a typed error.
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError>;
}

impl<P: ProviderClient> ProviderClient for &P {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        P::call(self, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderErrorKind, ProviderResponse};

    struct OkProvider;

    impl ProviderClient for OkProvider {
        fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Ok(ProviderResponse {
                content: format!("echo: {}", request.prompt),
            })
        }
    }

    struct RetryableProvider;

    impl ProviderClient for RetryableProvider {
        fn call(&self, _request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Err(ProviderError {
                kind: ProviderErrorKind::Retryable,
                message: "timeout".to_string(),
            })
        }
    }

    struct TerminalProvider;

    impl ProviderClient for TerminalProvider {
        fn call(&self, _request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            Err(ProviderError {
                kind: ProviderErrorKind::Terminal,
                message: "unauthorized".to_string(),
            })
        }
    }

    #[test]
    fn ok_provider_returns_content() {
        let client = OkProvider;
        let resp = client
            .call(ProviderRequest {
                prompt: "hi".to_string(),
            })
            .unwrap();
        assert_eq!(resp.content, "echo: hi");
    }

    #[test]
    fn retryable_provider_returns_retryable_error() {
        let client = RetryableProvider;
        let err = client
            .call(ProviderRequest {
                prompt: "hi".to_string(),
            })
            .unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Retryable);
    }

    #[test]
    fn terminal_provider_returns_terminal_error() {
        let client = TerminalProvider;
        let err = client
            .call(ProviderRequest {
                prompt: "hi".to_string(),
            })
            .unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
    }
}
