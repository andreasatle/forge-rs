//! Provider boundary — typed interface for LLM calls.
//!
//! This module defines the request/response/error types and the
//! `ProviderClient` trait.  Concrete providers (Anthropic, OpenAI, Ollama, …)
//! are not implemented here; they arrive in later phases.

pub mod client;
pub mod types;

pub use client::ProviderClient;
pub use types::{ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse};
