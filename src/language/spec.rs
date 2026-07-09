//! Language specification types deserialized from YAML.

use serde::Deserialize;

use crate::roles::policy::RolePromptConfig;
use crate::validation::{CommandSpec, ValidationTargetRule};

/// Complete specification for a language plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageSpec {
    /// File extensions (without the leading dot, e.g. `"py"`, `"rs"`) this
    /// plugin applies to. Used by the project adapter to pick the right
    /// plugin for a node from the extensions of its target files.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Who the language plugin frames the model as when writing this
    /// language.
    #[serde(default)]
    pub identity: String,
    /// Ambient background about this language's tooling and conventions.
    #[serde(default)]
    pub context: String,
    /// Guidance injected into coding prompts for this language: what to do.
    #[serde(default)]
    pub instructions: String,
    /// Language-specific constraints injected into coding prompts, alongside
    /// but distinct from `instructions`: prohibitions and conventions
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
    /// Command that extracts a readable API summary (signatures, docstrings)
    /// from a single file. Run per file in the artifact workspace with the
    /// file path as the last argument; its stdout is the file's summary.
    #[serde(default)]
    pub api_summary: Option<CommandSpec>,
}

impl LanguageSpec {
    /// Return true when the validation spec declares that it runs tests.
    ///
    /// This is driven by the explicit `runs_tests` field in the language YAML
    /// rather than by inspecting command tokens.
    pub fn validation_includes_test_command(&self) -> bool {
        self.validation.runs_tests
    }

    /// This plugin's prompt sections, for composition into a role prompt
    /// alongside the generic and adapter layers — see
    /// [`crate::project::YamlProjectAdapter::with_plugin_prompt`].
    pub fn prompt_sections(&self) -> RolePromptConfig {
        RolePromptConfig {
            identity: self.identity.clone(),
            context: self.context.clone(),
            instructions: self.instructions.clone(),
            constraints: self.constraints.clone(),
        }
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
            extensions: vec![],
            identity: String::new(),
            context: String::new(),
            instructions: String::new(),
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
            api_summary: None,
        };
        assert!(
            spec.validation_includes_test_command(),
            "must return true when runs_tests is explicitly true"
        );
    }

    #[test]
    fn validation_includes_test_command_is_false_without_runs_tests() {
        let spec = LanguageSpec {
            extensions: vec![],
            identity: String::new(),
            context: String::new(),
            instructions: String::new(),
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
            api_summary: None,
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
    fn bundled_language_specs_declare_api_summary() {
        for language in ["rust", "python"] {
            let spec =
                language_spec(language).unwrap_or_else(|| panic!("{language} spec must load"));
            assert!(
                spec.api_summary.is_some(),
                "{language} plugin must configure api_summary"
            );
        }
    }

    #[test]
    fn rust_api_summary_command_extracts_pub_signatures_from_a_real_file() {
        // Invariant: the shipped rust.yaml api_summary command must actually
        // extract pub fn/struct/enum/trait signatures when run against a real
        // file, not just parse as valid YAML.
        let spec = language_spec("rust").expect("rust spec must load");
        let command = spec
            .api_summary
            .expect("rust spec must configure api_summary");

        let dir = std::env::temp_dir().join(format!(
            "forge-rust-api-summary-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
        std::fs::write(
            dir.join("sample.rs"),
            "pub struct Foo;\n\nfn private() {}\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .expect("failed to write sample file");

        let output = std::process::Command::new(&command.program)
            .args(&command.args)
            .arg("sample.rs")
            .current_dir(&dir)
            .output()
            .expect("api_summary command must run");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

        std::fs::remove_dir_all(&dir).ok();

        assert!(
            output.status.success(),
            "api_summary command must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("pub struct Foo"), "got: {stdout}");
        assert!(
            stdout.contains("pub fn add(a: i32, b: i32) -> i32"),
            "got: {stdout}"
        );
        assert!(!stdout.contains("private"), "got: {stdout}");
    }

    #[test]
    fn constraints_defaults_to_empty_when_omitted_from_yaml() {
        // Invariant: constraints is optional in language YAML — a spec that
        // omits it still parses, with constraints defaulting to empty rather
        // than failing to parse.
        let yaml = r#"
identity: "guidance"
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
identity: "guidance"
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
identity: "guidance"
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
