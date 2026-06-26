//! Language specification types deserialized from YAML.

use serde::Deserialize;

use crate::validation::CommandSpec;

/// Complete specification for a language plugin.
#[derive(Debug, Deserialize)]
pub struct LanguageSpec {
    /// Short guidance injected into coding prompts for this language.
    pub prompt_guidance: String,
    /// Commands run once to initialize a new project workspace.
    pub init: LanguageInitSpec,
    /// Commands run to validate a workspace before integration.
    pub validation: LanguageValidationSpec,
}

/// Init-phase command list for a language.
#[derive(Debug, Deserialize)]
pub struct LanguageInitSpec {
    /// Ordered commands executed during project initialization.
    pub commands: Vec<CommandSpec>,
}

/// Validation-phase command list for a language.
#[derive(Debug, Deserialize)]
pub struct LanguageValidationSpec {
    /// Ordered commands executed to validate the workspace.
    pub commands: Vec<CommandSpec>,
}
