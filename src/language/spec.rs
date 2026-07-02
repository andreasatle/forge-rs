//! Language specification types deserialized from YAML.

use serde::Deserialize;

use crate::validation::{CommandSpec, ValidationTargetRule};

/// Complete specification for a language plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageSpec {
    /// Short guidance injected into coding prompts for this language.
    pub prompt_guidance: String,
    /// Language-specific constraints injected into coding prompts, alongside
    /// but distinct from `prompt_guidance`: prohibitions and conventions
    /// (e.g. import style, test naming, inline vs. separate test files)
    /// rather than general guidance.
    #[serde(default)]
    pub constraints: String,
    /// Commands run once to initialize a new project workspace.
    pub init: LanguageInitSpec,
    /// Commands run to validate a workspace before integration.
    pub validation: LanguageValidationSpec,
}

impl LanguageSpec {
    /// Return true when the validation spec declares that it runs tests.
    ///
    /// This is driven by the explicit `runs_tests` field in the language YAML
    /// rather than by inspecting command tokens.
    pub fn validation_includes_test_command(&self) -> bool {
        self.validation.runs_tests
    }
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
    /// When true, the validation suite runs tests and the planner must include
    /// test-related targets for code-changing plans.
    ///
    /// Set explicitly in the language YAML rather than inferred from command tokens.
    #[serde(default)]
    pub runs_tests: bool,
    /// Ordered commands executed to validate the workspace.
    pub commands: Vec<CommandSpec>,
    /// Ordered rules deriving validation targets (e.g. test files) from
    /// source targets named in a task.
    #[serde(default)]
    pub validation_targets: Vec<ValidationTargetRule>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::registry::language_spec;
    use crate::validation::ValidationScope;

    #[test]
    fn validation_includes_test_command_is_true_when_runs_tests_set() {
        let spec = LanguageSpec {
            prompt_guidance: String::new(),
            constraints: String::new(),
            init: LanguageInitSpec {
                gitignore: vec![],
                commands: vec![],
            },
            validation: LanguageValidationSpec {
                runs_tests: true,
                commands: vec![],
                validation_targets: vec![],
            },
        };
        assert!(
            spec.validation_includes_test_command(),
            "must return true when runs_tests is explicitly true"
        );
    }

    #[test]
    fn validation_includes_test_command_is_false_without_runs_tests() {
        let spec = LanguageSpec {
            prompt_guidance: String::new(),
            constraints: String::new(),
            init: LanguageInitSpec {
                gitignore: vec![],
                commands: vec![],
            },
            validation: LanguageValidationSpec {
                runs_tests: false,
                commands: vec![CommandSpec {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string()],
                    when_files_present: vec![],
                    scope: ValidationScope::Workspace,
                }],
                validation_targets: vec![],
            },
        };
        assert!(
            !spec.validation_includes_test_command(),
            "must return false when runs_tests is not set, even if commands include 'test'"
        );
    }

    #[test]
    fn rust_language_spec_declares_runs_tests() {
        let spec = language_spec("rust").expect("rust spec must load");
        assert!(
            spec.validation_includes_test_command(),
            "rust validation spec must declare runs_tests: true"
        );
    }

    #[test]
    fn python_language_spec_declares_runs_tests() {
        let spec = language_spec("python").expect("python spec must load");
        assert!(
            spec.validation_includes_test_command(),
            "python validation spec must declare runs_tests: true"
        );
    }

    #[test]
    fn constraints_defaults_to_empty_when_omitted_from_yaml() {
        // Invariant: constraints is optional in language YAML — a spec that
        // omits it still parses, with constraints defaulting to empty rather
        // than failing to parse.
        let yaml = r#"
prompt_guidance: "guidance"
init:
  commands: []
validation:
  commands: []
"#;
        let spec: LanguageSpec =
            serde_yaml::from_str(yaml).expect("spec without constraints must parse");
        assert_eq!(spec.constraints, "");
    }
}
