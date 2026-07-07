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

impl<P: ProviderClient + ?Sized> ProviderClient for Box<P> {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        P::call(self, request)
    }
}
