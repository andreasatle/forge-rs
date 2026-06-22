//! Workspace validation before artifact integration.

use crate::artifacts::Workspace;

/// Outcome of a workspace validation pass.
pub struct ValidationResult {
    /// Whether validation passed.
    pub passed: bool,
    /// Human-readable summary of the result.
    pub summary: String,
}

/// Validates a workspace before artifact integration.
pub trait Validator {
    /// Inspect `workspace` and return whether it is safe to integrate.
    fn validate(&self, workspace: &Workspace) -> ValidationResult;
}

/// A no-op validator that always passes.
///
/// Used as the default when no validator is configured.
pub struct AlwaysPassValidator;

impl Validator for AlwaysPassValidator {
    fn validate(&self, _workspace: &Workspace) -> ValidationResult {
        ValidationResult {
            passed: true,
            summary: "validation passed".to_string(),
        }
    }
}
