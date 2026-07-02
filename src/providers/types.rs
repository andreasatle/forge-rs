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
