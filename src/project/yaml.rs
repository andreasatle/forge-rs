//! Project adapter driven entirely by a YAML-loaded [`ProjectAdapterConfig`].

use super::ProjectAdapter;
use super::yaml_config::{ProjectAdapterConfig, ValidationTargetRule};
use crate::roles::RolePolicy;
use crate::roles::policy::{
    DEFAULT_SYSTEM, PLANNER_PRODUCER_IDENTITY, PLANNER_PROTOCOL_FOOTER_WITH_OPERATION,
    WORK_PRODUCER_SYSTEM, WORKER_PRODUCER_IDENTITY,
};

/// A [`ProjectAdapter`] whose role prompts, context files, and validation
/// target rules all come from a [`ProjectAdapterConfig`], rather than being
/// hardcoded in Rust.
pub struct YamlProjectAdapter {
    config: ProjectAdapterConfig,
}

impl YamlProjectAdapter {
    /// Build an adapter from an already-parsed configuration.
    pub fn new(config: ProjectAdapterConfig) -> Self {
        Self { config }
    }

    /// Parse a [`ProjectAdapterConfig`] from a YAML string and build an adapter.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, serde_yaml::Error> {
        let config: ProjectAdapterConfig = serde_yaml::from_str(yaml)?;
        Ok(Self::new(config))
    }
}

impl ProjectAdapter for YamlProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        // Each YAML-configured prompt supplies only the project-specific
        // portion of the role's system prompt. The generic role-identity and
        // JSON-protocol portions are framework constants, composed here so
        // every adapter gets them uniformly without restating them in YAML.
        let prompts = &self.config.role_prompts;
        RolePolicy {
            planner_producer_system: format!(
                "{PLANNER_PRODUCER_IDENTITY}\n{}\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}",
                prompts.planner_producer
            ),
            worker_producer_system: format!(
                "{WORKER_PRODUCER_IDENTITY} {}\n{WORK_PRODUCER_SYSTEM}",
                prompts.worker_producer
            ),
            planner_critic_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.planner_critic),
            worker_critic_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.worker_critic),
            planner_referee_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.planner_referee),
            worker_referee_system: format!("{}\n{DEFAULT_SYSTEM}", prompts.worker_referee),
        }
    }

    fn context_file_names(&self) -> Vec<String> {
        self.config.context_files.clone()
    }

    fn required_validation_targets(&self, targets: &[String]) -> Vec<String> {
        targets
            .iter()
            .filter_map(|target| derive_validation_target(&self.config.validation_targets, target))
            .collect()
    }
}

/// Apply `rules` to a single `target` path, returning the derived validation
/// target if one applies.
///
/// A target is skipped (returns `None`) when its basename already matches
/// the *target* pattern of any rule — this marks it as a derived validation
/// file rather than a source that needs one. Otherwise the first rule whose
/// *source* pattern matches wins.
fn derive_validation_target(rules: &[ValidationTargetRule], target: &str) -> Option<String> {
    let path = target.replace('\\', "/");
    let (prefix, basename) = path
        .rsplit_once('/')
        .map(|(dir, name)| (format!("{dir}/"), name))
        .unwrap_or((String::new(), path.as_str()));

    let already_derived = rules
        .iter()
        .any(|rule| match_stem(basename, &rule.target).is_some());
    if already_derived {
        return None;
    }

    rules.iter().find_map(|rule| {
        let stem = match_stem(basename, &rule.pattern)?;
        Some(format!("{prefix}{}", rule.target.replace("{stem}", &stem)))
    })
}

/// Match `basename` against `pattern`, which must contain exactly one
/// `{stem}` placeholder. Returns the captured stem on success.
fn match_stem(basename: &str, pattern: &str) -> Option<String> {
    let (before, after) = pattern.split_once("{stem}")?;
    if basename.len() < before.len() + after.len() {
        return None;
    }
    if !basename.starts_with(before) || !basename.ends_with(after) {
        return None;
    }
    let stem = &basename[before.len()..basename.len() - after.len()];
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::yaml_config::RolePromptsConfig;

    fn role_prompts() -> RolePromptsConfig {
        RolePromptsConfig {
            planner_producer: "plan it".to_string(),
            worker_producer: "build it".to_string(),
            planner_critic: "review the plan".to_string(),
            worker_critic: "review the work".to_string(),
            planner_referee: "decide the plan".to_string(),
            worker_referee: "decide the work".to_string(),
        }
    }

    fn coding_style_rules() -> Vec<ValidationTargetRule> {
        vec![
            ValidationTargetRule {
                pattern: "{stem}.py".to_string(),
                target: "test_{stem}.py".to_string(),
            },
            ValidationTargetRule {
                pattern: "{stem}.rs".to_string(),
                target: "{stem}_test.rs".to_string(),
            },
            ValidationTargetRule {
                pattern: "{stem}.go".to_string(),
                target: "{stem}_test.go".to_string(),
            },
            ValidationTargetRule {
                pattern: "{stem}.js".to_string(),
                target: "{stem}.test.js".to_string(),
            },
        ]
    }

    fn adapter_with_rules(validation_targets: Vec<ValidationTargetRule>) -> YamlProjectAdapter {
        YamlProjectAdapter::new(ProjectAdapterConfig {
            role_prompts: role_prompts(),
            context_files: vec!["README.md".to_string()],
            validation_targets,
        })
    }

    // ── role_policy ───────────────────────────────────────────────────────────

    #[test]
    fn role_policy_maps_each_field_from_config() {
        // Invariant: every RolePolicy field is composed from the matching
        // RolePromptsConfig field plus the shared framework protocol
        // constants, with no field left hardcoded or swapped.
        let adapter = adapter_with_rules(vec![]);
        let policy = adapter.role_policy();
        assert_eq!(
            policy.planner_producer_system,
            format!(
                "{PLANNER_PRODUCER_IDENTITY}\nplan it\n{PLANNER_PROTOCOL_FOOTER_WITH_OPERATION}"
            )
        );
        assert_eq!(
            policy.worker_producer_system,
            format!("{WORKER_PRODUCER_IDENTITY} build it\n{WORK_PRODUCER_SYSTEM}")
        );
        assert_eq!(
            policy.planner_critic_system,
            format!("review the plan\n{DEFAULT_SYSTEM}")
        );
        assert_eq!(
            policy.worker_critic_system,
            format!("review the work\n{DEFAULT_SYSTEM}")
        );
        assert_eq!(
            policy.planner_referee_system,
            format!("decide the plan\n{DEFAULT_SYSTEM}")
        );
        assert_eq!(
            policy.worker_referee_system,
            format!("decide the work\n{DEFAULT_SYSTEM}")
        );
    }

    // ── context_file_names ────────────────────────────────────────────────────

    #[test]
    fn context_file_names_returns_configured_files() {
        let adapter = adapter_with_rules(vec![]);
        assert_eq!(adapter.context_file_names(), vec!["README.md".to_string()]);
    }

    #[test]
    fn context_file_names_empty_when_unconfigured() {
        let adapter = YamlProjectAdapter::new(ProjectAdapterConfig {
            role_prompts: role_prompts(),
            context_files: vec![],
            validation_targets: vec![],
        });
        assert!(adapter.context_file_names().is_empty());
    }

    // ── required_validation_targets ──────────────────────────────────────────

    #[test]
    fn required_validation_targets_applies_matching_rule() {
        // Invariant: each rule table case derives the expected target basename.
        let adapter = adapter_with_rules(coding_style_rules());
        let cases: &[(&str, &str)] = &[
            ("main.py", "test_main.py"),
            ("lib.rs", "lib_test.rs"),
            ("server.go", "server_test.go"),
            ("util.js", "util.test.js"),
        ];
        for (source, expected) in cases {
            assert_eq!(
                adapter.required_validation_targets(&[source.to_string()]),
                vec![expected.to_string()],
                "wrong validation target for {source}"
            );
        }
    }

    #[test]
    fn required_validation_targets_excludes_already_derived_files() {
        // Invariant: files whose basename already matches a rule's target
        // pattern are not source files themselves and produce no target.
        let adapter = adapter_with_rules(coding_style_rules());
        for derived in &[
            "test_main.py",
            "lib_test.rs",
            "server_test.go",
            "util.test.js",
        ] {
            let result = adapter.required_validation_targets(&[derived.to_string()]);
            assert!(
                result.is_empty(),
                "already-derived file {derived} must produce no target; got {result:?}"
            );
        }
    }

    #[test]
    fn required_validation_targets_excludes_files_matching_no_rule() {
        let adapter = adapter_with_rules(coding_style_rules());
        for non_code in &["README.md", "config.yaml", "Cargo.lock"] {
            let result = adapter.required_validation_targets(&[non_code.to_string()]);
            assert!(
                result.is_empty(),
                "non-matching file {non_code} must produce no target; got {result:?}"
            );
        }
    }

    #[test]
    fn required_validation_targets_preserves_directory_prefix() {
        let adapter = adapter_with_rules(coding_style_rules());
        assert_eq!(
            adapter.required_validation_targets(&["src/main.py".to_string()]),
            vec!["src/test_main.py".to_string()],
        );
        assert_eq!(
            adapter.required_validation_targets(&["pkg/server.go".to_string()]),
            vec!["pkg/server_test.go".to_string()],
        );
    }

    #[test]
    fn required_validation_targets_handles_multiple_sources_independently() {
        let adapter = adapter_with_rules(coding_style_rules());
        let mut result =
            adapter.required_validation_targets(&["main.py".to_string(), "utils.rs".to_string()]);
        result.sort();
        let mut expected = vec!["test_main.py".to_string(), "utils_test.rs".to_string()];
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn required_validation_targets_empty_input_returns_empty() {
        let adapter = adapter_with_rules(coding_style_rules());
        assert!(adapter.required_validation_targets(&[]).is_empty());
    }

    #[test]
    fn required_validation_targets_empty_rules_returns_empty() {
        // Invariant: with no configured rules, no target is ever derived.
        let adapter = adapter_with_rules(vec![]);
        assert!(
            adapter
                .required_validation_targets(&["main.py".to_string()])
                .is_empty()
        );
    }

    #[test]
    fn required_validation_targets_first_matching_rule_wins() {
        // Invariant: rules are tried in declared order; the first match applies.
        let adapter = adapter_with_rules(vec![
            ValidationTargetRule {
                pattern: "{stem}.py".to_string(),
                target: "first_{stem}.py".to_string(),
            },
            ValidationTargetRule {
                pattern: "{stem}.py".to_string(),
                target: "second_{stem}.py".to_string(),
            },
        ]);
        assert_eq!(
            adapter.required_validation_targets(&["main.py".to_string()]),
            vec!["first_main.py".to_string()],
        );
    }

    // ── from_yaml_str ─────────────────────────────────────────────────────────

    #[test]
    fn from_yaml_str_builds_working_adapter() {
        let yaml = r#"
role_prompts:
  planner_producer: "plan it"
  worker_producer: "build it"
  planner_critic: "review the plan"
  worker_critic: "review the work"
  planner_referee: "decide the plan"
  worker_referee: "decide the work"
context_files:
  - README.md
validation_targets:
  - pattern: "{stem}.py"
    target: "test_{stem}.py"
"#;
        let adapter = YamlProjectAdapter::from_yaml_str(yaml).unwrap();
        assert!(
            adapter
                .role_policy()
                .planner_producer_system
                .contains("plan it")
        );
        assert_eq!(adapter.context_file_names(), vec!["README.md".to_string()]);
        assert_eq!(
            adapter.required_validation_targets(&["main.py".to_string()]),
            vec!["test_main.py".to_string()],
        );
    }

    #[test]
    fn from_yaml_str_rejects_invalid_yaml() {
        let result = YamlProjectAdapter::from_yaml_str("not: valid: yaml: [");
        assert!(result.is_err(), "invalid YAML must return an error");
    }

    // ── match_stem ────────────────────────────────────────────────────────────

    #[test]
    fn match_stem_rejects_pattern_without_placeholder() {
        assert_eq!(match_stem("main.py", "main.py"), None);
    }

    #[test]
    fn match_stem_rejects_empty_capture() {
        // Invariant: a pattern that would capture an empty stem does not match.
        assert_eq!(match_stem(".py", "{stem}.py"), None);
    }

    #[test]
    fn match_stem_rejects_non_matching_basename() {
        assert_eq!(match_stem("main.rs", "{stem}.py"), None);
    }
}
