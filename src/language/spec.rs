//! Language specification types deserialized from YAML.

use serde::{Deserialize, Serialize};

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
    /// Rules deriving a target file from a task's bare name (see
    /// [`crate::artifacts::TaskRecord::name`]), for nodes spawned with no
    /// target file of their own to select a plugin or validation rule from.
    ///
    /// Used as the default for any worker role without its own override in
    /// `roles` (see [`Self::name_target_rules_for_role`]).
    #[serde(default)]
    pub name_target_rules: Vec<NameTargetRule>,
}

impl LanguageSpec {
    /// Return true when the validation spec declares that it runs tests.
    ///
    /// This is driven by the explicit `runs_tests` field in the language YAML
    /// rather than by inspecting command tokens.
    pub fn validation_includes_test_command(&self) -> bool {
        self.validation.runs_tests
    }

    /// Name-target rules that apply for `role`, mirroring how `roles`
    /// overrides `validation`: a role with its own non-empty
    /// `name_target_rules` uses those instead of the plugin-level default —
    /// e.g. a `tester` role deriving `tests/test_{name}.py` while the
    /// plugin's own default derives `src/{name}.py` for every other role.
    ///
    /// Falls back to the plugin-level [`Self::name_target_rules`] when `role`
    /// is `None`, matches no entry in `roles`, or that entry has no
    /// `name_target_rules` of its own.
    pub fn name_target_rules_for_role(&self, role: Option<&str>) -> &[NameTargetRule] {
        role.and_then(|role| self.roles.iter().find(|r| r.role == role))
            .map(|r| r.name_target_rules.as_slice())
            .filter(|rules| !rules.is_empty())
            .unwrap_or(&self.name_target_rules)
    }

    /// This plugin's prompt sections, for composition into a role prompt
    /// alongside the generic and adapter layers — selected per node from the
    /// node's own target files, see [`crate::language::select_plugin`].
    pub fn prompt_sections(&self) -> RolePromptConfig {
        RolePromptConfig {
            identity: self.identity.clone(),
            context: self.context.clone(),
            instructions: self.instructions.clone(),
            constraints: self.constraints.clone(),
        }
    }
}

/// A rule deriving a target file from a task's bare name, independent of
/// any target file the task already carries.
///
/// `pattern` is matched against the whole name; it contains exactly one
/// `{name}` placeholder and matches when the name starts with the text
/// before it and ends with the text after it. The captured middle section
/// is substituted into `target` to derive the target path.
///
/// Example: `pattern: "{name}"`, `target: "src/{name}.py"` derives
/// `src/add_parser.py` from the name `add_parser`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NameTargetRule {
    /// Task-name pattern, e.g. `"{name}"`.
    pub pattern: String,
    /// Target file pattern, e.g. `"src/{name}.py"`.
    pub target: String,
}

impl NameTargetRule {
    /// Applies this rule to `name`, returning the derived target path if
    /// `name` matches `pattern` as a whole (not a substring). See the type
    /// docs for the matching rule.
    fn apply(&self, name: &str) -> Option<String> {
        let (before, after) = self.pattern.split_once("{name}")?;
        if name.len() < before.len() + after.len() {
            return None;
        }
        if !name.starts_with(before) || !name.ends_with(after) {
            return None;
        }
        let captured = &name[before.len()..name.len() - after.len()];
        if captured.is_empty() {
            return None;
        }
        Some(self.target.replace("{name}", captured))
    }
}

/// Applies `rules` in order to `name`, returning the first matching rule's
/// derived target path, or `None` when no rule's pattern matches.
pub fn derive_target_from_name(rules: &[NameTargetRule], name: &str) -> Option<String> {
    rules.iter().find_map(|rule| rule.apply(name))
}

/// A worker role's validation override for a language plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageRoleConfig {
    /// The worker role name this override applies to (e.g. `"tester"`).
    pub role: String,
    /// Validation spec used for nodes assigned this role, replacing the
    /// language's default `validation` spec entirely.
    pub validation: LanguageValidationSpec,
    /// Name-target rules used for nodes assigned this role, replacing the
    /// plugin-level [`LanguageSpec::name_target_rules`] entirely — same
    /// override semantics as `validation`. Empty by default, in which case
    /// [`LanguageSpec::name_target_rules_for_role`] falls back to the
    /// plugin-level rules.
    #[serde(default)]
    pub name_target_rules: Vec<NameTargetRule>,
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
            name_target_rules: vec![],
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
            name_target_rules: vec![],
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

    #[test]
    fn name_target_rules_defaults_to_empty_when_omitted_from_yaml() {
        // Invariant: name_target_rules is optional — a language spec with no
        // name-derived target rules still parses, defaulting to empty.
        let yaml = r#"
identity: "guidance"
init:
  commands: []
validation:
  commands: []
"#;
        let spec: LanguageSpec =
            serde_yaml::from_str(yaml).expect("spec without name_target_rules must parse");
        assert!(spec.name_target_rules.is_empty());
    }

    #[test]
    fn name_target_rules_parse_pattern_and_target_from_yaml() {
        // Invariant: each name_target_rules entry carries its own pattern and
        // target strings, independent of validation_targets.
        let yaml = r#"
identity: "guidance"
init:
  commands: []
validation:
  commands: []
name_target_rules:
  - pattern: "{name}"
    target: "src/{name}.py"
"#;
        let spec: LanguageSpec =
            serde_yaml::from_str(yaml).expect("spec with name_target_rules must parse");
        assert_eq!(spec.name_target_rules.len(), 1);
        assert_eq!(spec.name_target_rules[0].pattern, "{name}");
        assert_eq!(spec.name_target_rules[0].target, "src/{name}.py");
    }

    #[test]
    fn derive_target_from_name_substitutes_captured_middle() {
        // Invariant: the text captured between a rule's pattern prefix/suffix
        // is substituted verbatim into the target pattern's {name} slot.
        let rules = vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "src/{name}.py".to_string(),
        }];
        assert_eq!(
            derive_target_from_name(&rules, "add_parser"),
            Some("src/add_parser.py".to_string())
        );
    }

    #[test]
    fn derive_target_from_name_returns_none_when_no_rule_matches() {
        // Invariant: a name that fits no configured rule's pattern derives
        // no target — callers must not guess a fallback.
        let rules = vec![NameTargetRule {
            pattern: "test_{name}".to_string(),
            target: "tests/test_{name}.py".to_string(),
        }];
        assert_eq!(derive_target_from_name(&rules, "add_parser"), None);
    }

    #[test]
    fn derive_target_from_name_returns_none_for_empty_rules() {
        // Invariant: a team/spec with no configured name_target_rules never
        // derives a target, regardless of the name.
        assert_eq!(derive_target_from_name(&[], "add_parser"), None);
    }

    #[test]
    fn derive_target_from_name_uses_first_matching_rule() {
        // Invariant: rules are tried in order and the first match wins, same
        // as validation_targets' derivation semantics.
        let rules = vec![
            NameTargetRule {
                pattern: "{name}".to_string(),
                target: "src/{name}.rs".to_string(),
            },
            NameTargetRule {
                pattern: "{name}".to_string(),
                target: "src/{name}.py".to_string(),
            },
        ];
        assert_eq!(
            derive_target_from_name(&rules, "add_parser"),
            Some("src/add_parser.rs".to_string())
        );
    }

    #[test]
    fn name_target_rules_for_role_falls_back_to_plugin_default() {
        // Invariant: a role with no override, or no role at all, uses the
        // plugin-level name_target_rules — this is what keeps an
        // implementer-only (or role-less) adapter deriving the same target
        // it always has.
        let default_rules = vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "src/{name}.py".to_string(),
        }];
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
            roles: vec![LanguageRoleConfig {
                role: "implementer".to_string(),
                validation: LanguageValidationSpec {
                    runs_tests: true,
                    commands: vec![],
                    validation_targets: vec![],
                },
                name_target_rules: vec![],
            }],
            api_summary: None,
            name_target_rules: default_rules.clone(),
        };
        assert_eq!(spec.name_target_rules_for_role(None), default_rules);
        assert_eq!(
            spec.name_target_rules_for_role(Some("implementer")),
            default_rules
        );
        assert_eq!(
            spec.name_target_rules_for_role(Some("unknown_role")),
            default_rules
        );
    }

    #[test]
    fn name_target_rules_for_role_uses_role_override_when_present() {
        // Invariant: a role with its own non-empty name_target_rules (e.g. a
        // tester deriving a test file) replaces the plugin-level default
        // entirely — the two never merge.
        let tester_rules = vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "tests/test_{name}.py".to_string(),
        }];
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
            roles: vec![LanguageRoleConfig {
                role: "tester".to_string(),
                validation: LanguageValidationSpec {
                    runs_tests: false,
                    commands: vec![],
                    validation_targets: vec![],
                },
                name_target_rules: tester_rules.clone(),
            }],
            api_summary: None,
            name_target_rules: vec![NameTargetRule {
                pattern: "{name}".to_string(),
                target: "src/{name}.py".to_string(),
            }],
        };
        assert_eq!(
            spec.name_target_rules_for_role(Some("tester")),
            tester_rules
        );
    }

    #[test]
    fn bundled_language_specs_declare_name_target_rules() {
        for language in ["rust", "python"] {
            let spec =
                language_spec(language).unwrap_or_else(|| panic!("{language} spec must load"));
            assert!(
                !spec.name_target_rules.is_empty(),
                "{language} plugin must configure name_target_rules"
            );
        }
    }

    #[test]
    fn bundled_language_specs_derive_a_distinct_tester_target_from_the_default() {
        // Invariant: the shipped plugins' tester role derives a test-file
        // target distinct from the plugin-level default (which derives the
        // source file) — this is what lets a create_test-style adapter's
        // ForTasks-spawned Work node target a test file instead of colliding
        // with the implementer's source-file target.
        for language in ["rust", "python"] {
            let spec =
                language_spec(language).unwrap_or_else(|| panic!("{language} spec must load"));
            let default_target = derive_target_from_name(&spec.name_target_rules, "example")
                .unwrap_or_else(|| panic!("{language} default name_target_rules must match"));
            let tester_target =
                derive_target_from_name(spec.name_target_rules_for_role(Some("tester")), "example")
                    .unwrap_or_else(|| panic!("{language} tester name_target_rules must match"));
            assert_ne!(
                default_target, tester_target,
                "{language} tester role must derive a distinct target from the plugin default"
            );
        }
    }
}
