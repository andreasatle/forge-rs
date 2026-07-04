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
    /// Commands run to validate a workspace before integration. Used as the
    /// default for any worker role without an entry in `roles`.
    pub validation: LanguageValidationSpec,
    /// Per-worker-role validation overrides, keyed by role name (e.g.
    /// `"tester"`, `"implementer"`) to match a
    /// [`crate::project::WorkerRoleConfig::role`] defined by the project
    /// adapter. A role without an entry here falls back to `validation`.
    #[serde(default)]
    pub roles: Vec<LanguageRoleConfig>,
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

/// A worker role's validation override for a language plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageRoleConfig {
    /// The worker role name this override applies to (e.g. `"tester"`).
    pub role: String,
    /// Validation spec used for nodes assigned this role, replacing the
    /// language's default `validation` spec entirely.
    pub validation: LanguageValidationSpec,
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
            roles: vec![],
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
            roles: vec![],
        };
        assert!(
            !spec.validation_includes_test_command(),
            "must return false when runs_tests is not set, even if commands include 'test'"
        );
    }

    #[test]
    fn bundled_language_specs_declare_runs_tests() {
        for language in ["rust", "python"] {
            let spec =
                language_spec(language).unwrap_or_else(|| panic!("{language} spec must load"));
            assert!(
                spec.validation_includes_test_command(),
                "{language} validation spec must declare runs_tests: true"
            );
        }
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

    #[test]
    fn roles_defaults_to_empty_when_omitted_from_yaml() {
        // Invariant: roles is optional — a language spec with no per-role
        // validation overrides still parses, with roles defaulting to empty.
        let yaml = r#"
prompt_guidance: "guidance"
init:
  commands: []
validation:
  commands: []
"#;
        let spec: LanguageSpec = serde_yaml::from_str(yaml).expect("spec without roles must parse");
        assert!(spec.roles.is_empty());
    }

    #[test]
    fn roles_parse_their_own_validation_spec() {
        // Invariant: each entry in roles carries its own role name and a
        // full LanguageValidationSpec, independent of the default validation
        // spec.
        let yaml = r#"
prompt_guidance: "guidance"
init:
  commands: []
validation:
  runs_tests: true
  commands:
    - program: "full"
      args: []
roles:
  - role: tester
    validation:
      runs_tests: false
      commands:
        - program: "reduced"
          args: []
"#;
        let spec: LanguageSpec = serde_yaml::from_str(yaml).expect("spec with roles must parse");
        assert_eq!(spec.roles.len(), 1);
        assert_eq!(spec.roles[0].role, "tester");
        assert!(!spec.roles[0].validation.runs_tests);
        assert_eq!(spec.roles[0].validation.commands[0].program, "reduced");
        assert!(spec.validation.runs_tests);
        assert_eq!(spec.validation.commands[0].program, "full");
    }
}
