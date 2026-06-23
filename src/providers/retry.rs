//! Retry wrapper around any [`ProviderClient`].

use crate::providers::client::ProviderClient;
use crate::providers::types::{
    ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse,
};

/// Wraps a [`ProviderClient`] and retries on [`ProviderErrorKind::Retryable`] errors.
///
/// Terminal errors are returned immediately without retrying.
/// After exhausting `max_attempts`, the last retryable error is returned unchanged.
pub struct RetryingProvider<P> {
    inner: P,
    max_attempts: usize,
}

impl<P> RetryingProvider<P> {
    /// Create a new `RetryingProvider` that will attempt up to `max_attempts` calls.
    pub fn new(inner: P, max_attempts: usize) -> Self {
        Self {
            inner,
            max_attempts,
        }
    }
}

impl<P: ProviderClient> ProviderClient for RetryingProvider<P> {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        if self.max_attempts == 0 {
            return Err(ProviderError {
                kind: ProviderErrorKind::Terminal,
                message: "RetryingProvider max_attempts must be >= 1".to_string(),
            });
        }
        let mut last_error: Option<ProviderError> = None;
        for _ in 0..self.max_attempts {
            match self.inner.call(request.clone()) {
                Ok(resp) => return Ok(resp),
                Err(err) if err.kind == ProviderErrorKind::Terminal => return Err(err),
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap()) // safe: loop ran at least once (max_attempts > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct ScriptedProvider {
        script: RefCell<Vec<Result<ProviderResponse, ProviderError>>>,
        call_count: Cell<usize>,
    }

    impl ScriptedProvider {
        fn new(script: Vec<Result<ProviderResponse, ProviderError>>) -> Self {
            Self {
                script: RefCell::new(script),
                call_count: Cell::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.call_count.get()
        }
    }

    impl ProviderClient for ScriptedProvider {
        fn call(&self, _request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            self.call_count.set(self.call_count.get() + 1);
            self.script.borrow_mut().remove(0)
        }
    }

    fn ok(content: &str) -> Result<ProviderResponse, ProviderError> {
        Ok(ProviderResponse {
            content: content.to_string(),
        })
    }

    fn retryable(msg: &str) -> Result<ProviderResponse, ProviderError> {
        Err(ProviderError {
            kind: ProviderErrorKind::Retryable,
            message: msg.to_string(),
        })
    }

    fn terminal(msg: &str) -> Result<ProviderResponse, ProviderError> {
        Err(ProviderError {
            kind: ProviderErrorKind::Terminal,
            message: msg.to_string(),
        })
    }

    fn req() -> ProviderRequest {
        ProviderRequest {
            prompt: "test".to_string(),
        }
    }

    #[test]
    fn success_without_retry() {
        let inner = ScriptedProvider::new(vec![ok("done")]);
        let client = RetryingProvider::new(inner, 3);
        let resp = client.call(req()).unwrap();
        assert_eq!(resp.content, "done");
        assert_eq!(client.inner.call_count(), 1);
    }

    #[test]
    fn retryable_error_then_success() {
        let inner =
            ScriptedProvider::new(vec![retryable("timeout"), retryable("timeout"), ok("done")]);
        let client = RetryingProvider::new(inner, 5);
        let resp = client.call(req()).unwrap();
        assert_eq!(resp.content, "done");
        assert_eq!(client.inner.call_count(), 3);
    }

    #[test]
    fn terminal_error_stops_immediately() {
        let inner = ScriptedProvider::new(vec![terminal("unauthorized")]);
        let client = RetryingProvider::new(inner, 5);
        let err = client.call(req()).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
        assert_eq!(err.message, "unauthorized");
        assert_eq!(client.inner.call_count(), 1);
    }

    #[test]
    fn retryable_error_exhaustion() {
        let inner = ScriptedProvider::new(vec![
            retryable("rate limited"),
            retryable("rate limited"),
            retryable("rate limited"),
        ]);
        let client = RetryingProvider::new(inner, 3);
        let err = client.call(req()).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Retryable);
        assert_eq!(err.message, "rate limited");
        assert_eq!(client.inner.call_count(), 3);
    }

    #[test]
    fn retrying_provider_rejects_zero_attempts_without_panic() {
        let inner = ScriptedProvider::new(vec![]);
        let client = RetryingProvider::new(inner, 0);
        let err = client.call(req()).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
        assert!(
            err.message.contains("max_attempts"),
            "error must mention max_attempts, got: {}",
            err.message
        );
        assert_eq!(
            client.inner.call_count(),
            0,
            "inner must not be called when max_attempts is zero"
        );
    }

    #[test]
    fn mixed_retry_then_terminal() {
        let inner = ScriptedProvider::new(vec![
            retryable("timeout"),
            retryable("timeout"),
            terminal("unauthorized"),
        ]);
        let client = RetryingProvider::new(inner, 5);
        let err = client.call(req()).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
        assert_eq!(err.message, "unauthorized");
        assert_eq!(client.inner.call_count(), 3);
    }
}
