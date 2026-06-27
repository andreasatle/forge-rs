//! Language specification types deserialized from YAML.

use serde::Deserialize;

use crate::validation::CommandSpec;

/// Complete specification for a language plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageSpec {
    /// Short guidance injected into coding prompts for this language.
    pub prompt_guidance: String,
    /// Commands run once to initialize a new project workspace.
    pub init: LanguageInitSpec,
    /// Commands run to validate a workspace before integration.
    pub validation: LanguageValidationSpec,
}

impl LanguageSpec {
    /// Return true when the validation command list appears to run tests.
    ///
    /// The registry stays tool-agnostic: this checks command tokens for common
    /// test-command names rather than recognizing a specific language tool.
    pub fn validation_includes_test_command(&self) -> bool {
        self.validation.commands.iter().any(command_is_test_like)
    }
}

/// Return true when a command token looks like a test runner or test subcommand.
pub fn command_is_test_like(command: &CommandSpec) -> bool {
    std::iter::once(command.program.as_str())
        .chain(command.args.iter().map(String::as_str))
        .any(token_is_test_like)
}

fn token_is_test_like(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower == "test" || lower.ends_with("test") || lower.ends_with("tests")
}

/// Init-phase command list for a language.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageInitSpec {
    /// Patterns appended to `.gitignore` before init commands run.
    ///
    /// Prevents generated artifacts (e.g. virtual environments) from being
    /// staged by `git add --all` after the language initializer runs.
    #[serde(default)]
    pub gitignore: Vec<String>,
    /// Ordered commands executed during project initialization.
    pub commands: Vec<CommandSpec>,
}

/// Validation-phase command list for a language.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageValidationSpec {
    /// Ordered commands executed to validate the workspace.
    pub commands: Vec<CommandSpec>,
}
