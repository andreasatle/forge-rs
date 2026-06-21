//! Provider boundary — typed interface for LLM calls.
//!
//! This module defines the request/response/error types and the
//! `ProviderClient` trait.  Concrete providers live in their own submodules.

pub mod client;
pub mod ollama;
pub mod retry;
pub mod types;

pub use client::ProviderClient;
pub use ollama::OllamaProvider;
pub use retry::RetryingProvider;
pub use types::{ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse};
