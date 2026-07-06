use super::*;
use crate::config::ValidationConfig;
use crate::language::registry::{language_spec, register_test_language_spec};
use crate::language::spec::LanguageRoleConfig;
use crate::language::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};
use std::path::PathBuf;

fn adapter_path(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("adapters")
        .join(name)
}

fn plugin_path(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(name)
}

fn builder<'a>(
    adapter: &str,
    plugin: Option<&str>,
    validation: Option<&'a ValidationConfig>,
) -> ProjectRuntimeSetupBuilder<'a> {
    ProjectRuntimeSetupBuilder::new(
        &adapter_path(adapter),
        plugin.map(plugin_path).as_deref(),
        validation,
    )
    .unwrap()
}

// ── adapter selection ────────────────────────────────────────────────────

#[test]
fn runtime_selects_coding_adapter() {
    let policy = builder("coding.yaml", None, None).role_policy();
    assert!(
        policy.planner_producer_system.contains("software planning"),
        "coding adapter must produce software-planning planner prompt; got:\n{}",
        policy.planner_producer_system
    );
    assert!(
        policy
            .worker_producer_system
            .contains("software implementation"),
        "coding adapter must produce software-implementation worker prompt; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn runtime_selects_coding_tdd_adapter() {
    let policy = builder("coding_tdd.yaml", None, None).role_policy();
    assert!(
        policy
            .planner_producer_system
            .contains("before the implementation nodes"),
        "coding_tdd adapter must select the TDD planner prompt; got:\n{}",
        policy.planner_producer_system
    );
    assert!(
        policy
            .worker_producer_system
            .contains("import the functions under test"),
        "coding_tdd adapter must select the TDD worker prompt; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn unknown_adapter_fails_loudly() {
    let result = ProjectRuntimeSetupBuilder::new(&adapter_path("bogus.yaml"), None, None);
    assert!(result.is_err(), "unrecognised adapter must be a hard error");
}

#[test]
fn runtime_role_policy_includes_language_guidance_when_plugin_set() {
    let policy = builder("coding.yaml", Some("rust.yaml"), None).role_policy();
    let expected = language_spec("rust")
        .expect("rust spec must load")
        .prompt_guidance;
    assert_eq!(
        policy.language_guidance,
        Some(expected),
        "role policy must carry the rust language spec's prompt_guidance"
    );
}

#[test]
fn runtime_role_policy_has_no_language_guidance_when_plugin_unset() {
    let policy = builder("coding.yaml", None, None).role_policy();
    assert_eq!(
        policy.language_guidance, None,
        "role policy must have no language guidance when no plugin is configured"
    );
}

#[test]
fn runtime_role_policy_includes_language_constraints_when_plugin_set() {
    let policy = builder("coding.yaml", Some("rust.yaml"), None).role_policy();
    let expected = language_spec("rust")
        .expect("rust spec must load")
        .constraints;
    assert_eq!(
        policy.language_constraints,
        Some(expected),
        "role policy must carry the rust language spec's constraints"
    );
}

#[test]
fn runtime_role_policy_has_no_language_constraints_when_plugin_unset() {
    let policy = builder("coding.yaml", None, None).role_policy();
    assert_eq!(
        policy.language_constraints, None,
        "role policy must have no language constraints when no plugin is configured"
    );
}

#[test]
fn unknown_plugin_fails_loudly() {
    let result = ProjectRuntimeSetupBuilder::new(
        &adapter_path("coding.yaml"),
        Some(&plugin_path("bogus.yaml")),
        None,
    );
    assert!(result.is_err(), "unrecognised plugin must be a hard error");
}

#[test]
fn adapter_role_missing_from_plugin_roles_fails_loudly() {
    // Invariant: when a plugin is configured, every worker role the adapter
    // defines must have a matching entry in the plugin's `roles` list — a
    // plugin that only covers some of the adapter's roles is a hard error at
    // config load time, not a silent fallback to default validation at run
    // time.
    let id = "test-plugin-missing-tester-role";
    register_test_language_spec(
        id,
        LanguageSpec {
            prompt_guidance: String::new(),
            constraints: String::new(),
            init: LanguageInitSpec {
                gitignore: vec![],
                commands: vec![],
            },
            validation: LanguageValidationSpec {
                runs_tests: false,
                commands: vec![],
                validation_targets: vec![],
            },
            roles: vec![LanguageRoleConfig {
                role: "implementer".to_string(),
                validation: LanguageValidationSpec {
                    runs_tests: false,
                    commands: vec![],
                    validation_targets: vec![],
                },
            }],
            api_summary: None,
        },
    );

    let result = ProjectRuntimeSetupBuilder::new(
        &adapter_path("coding_tdd.yaml"),
        Some(std::path::Path::new(id)),
        None,
    );
    let err = match result {
        Ok(_) => panic!("adapter role missing from plugin roles must be a hard error"),
        Err(e) => e.to_string(),
    };

    assert_eq!(
        err,
        format!("adapter role 'tester' is not defined in plugin '{id}'")
    );
}

// ── make_validator ───────────────────────────────────────────────────────

#[test]
fn runtime_uses_always_pass_when_validation_absent() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    let validator = builder("coding.yaml", None, None).validator();
    let result = validator.validate(&ws);
    assert!(
        result.passed,
        "absent validation config must yield a passing validator"
    );
}

#[test]
fn runtime_uses_command_validator_when_configured() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    // A failing command proves the CommandValidator is active, not AlwaysPassValidator.
    let config = ValidationConfig {
        commands: vec!["false".to_string()],
        timeout_seconds: None,
    };
    let validator = builder("coding.yaml", None, Some(&config)).validator();
    let result = validator.validate(&ws);
    assert!(
        !result.passed,
        "configured command validator must run commands and fail on non-zero exit"
    );
}

#[test]
fn runtime_language_validator_uses_language_spec_commands() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    // Rust language spec provides validation commands — they won't pass
    // in a non-Rust workspace, but we can verify a CommandValidator is returned
    // by checking it is not the AlwaysPassValidator (which always passes).
    //
    // We use "rust.yaml" which provides cargo commands; in a bare temp dir they
    // will fail, confirming a real CommandValidator was wired up.
    let validator = builder("coding.yaml", Some("rust.yaml"), None).validator();
    let result = validator.validate(&ws);
    // cargo fmt --check, cargo check, cargo test will all fail in a temp dir
    assert!(
        !result.passed,
        "rust language validator must run cargo commands that fail in a temp dir; got: {}",
        result.summary
    );
}

#[test]
fn runtime_backward_compat_validation_yaml_translates_to_sh_wrapper() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    // Raw YAML commands are wrapped in sh -c for backward compatibility.
    // A passing shell command confirms the translation works.
    let config = ValidationConfig {
        commands: vec!["true".to_string()],
        timeout_seconds: None,
    };
    let validator = builder("coding.yaml", None, Some(&config)).validator();
    let result = validator.validate(&ws);
    assert!(
        result.passed,
        "sh-wrapped 'true' must pass via backward-compat translation; got: {}",
        result.summary
    );
}

// ── validation_command_is_test_like ──────────────────────────────────────

#[test]
fn validation_command_is_test_like_classifies_commands() {
    let cases = [
        ("cargo test", true),
        ("uv run pytest", true),
        ("test", true),
        ("cargo fmt --check", false),
        ("uv run ruff check .", false),
    ];
    for (command, expected) in cases {
        assert_eq!(
            ProjectRuntimeSetupBuilder::validation_command_is_test_like(command),
            expected,
            "command: {command:?}"
        );
    }
}

// ── project_requires_tests ───────────────────────────────────────────────

#[test]
fn project_requires_tests_reflects_validation_commands() {
    let cases = [("cargo test", true), ("cargo fmt --check", false)];
    for (command, expected) in cases {
        let config = ValidationConfig {
            commands: vec![command.to_string()],
            timeout_seconds: None,
        };
        assert_eq!(
            builder("coding.yaml", None, Some(&config)).project_requires_tests(),
            expected,
            "command: {command:?}"
        );
    }
}

#[test]
fn project_requires_tests_false_when_no_validation() {
    assert!(
        !builder("coding.yaml", None, None).project_requires_tests(),
        "absent validation must set requires_tests = false"
    );
}

// ── ProjectRuntimeSetup::build ───────────────────────────────────────────

#[test]
fn build_derives_validator_and_validation_plan_from_language() {
    let setup = ProjectRuntimeSetup::build(
        &adapter_path("coding.yaml"),
        Some(&plugin_path("rust.yaml")),
        None,
    )
    .unwrap();
    assert!(
        (setup.validation_plan_for_role_fn)(None).is_some(),
        "a configured language plugin must produce a validation plan"
    );
    assert!(
        setup
            .role_policy
            .language_guidance
            .is_some_and(|g| !g.is_empty()),
        "build() must thread language guidance into the role policy"
    );
}

#[test]
fn validation_plan_for_role_uses_python_tester_role_override() {
    // Invariant: a `tester`-role Work node gets the python plugin's
    // role-specific validation plan (ruff check only) rather than the
    // plugin's default plan (ruff + pyright + pytest), so tester nodes
    // aren't required to pass the full test suite before their own test
    // files exist.
    let setup = ProjectRuntimeSetup::build(
        &adapter_path("coding.yaml"),
        Some(&plugin_path("python.yaml")),
        None,
    )
    .unwrap();
    let plan = (setup.validation_plan_for_role_fn)(Some("tester"))
        .expect("python plugin's tester role override must produce a validation plan");
    assert_eq!(
        plan.steps.len(),
        1,
        "python tester validation plan must contain exactly one step; got: {:?}",
        plan.steps
    );
    assert!(
        plan.steps[0].command.contains(&"ruff".to_string()),
        "python tester validation plan must run ruff; got: {:?}",
        plan.steps[0].command
    );
    assert!(
        !plan.steps[0].command.contains(&"pytest".to_string()),
        "python tester validation plan must not run pytest"
    );
}

#[test]
fn validation_plan_for_role_falls_back_to_default_when_role_has_no_override() {
    // Invariant: a role absent from the language spec's `roles` list (e.g.
    // any role that isn't one of rust's configured overrides) must not
    // silently drop validation — it falls back to the same default plan
    // used for nodes with no role assigned.
    let setup = ProjectRuntimeSetup::build(
        &adapter_path("coding.yaml"),
        Some(&plugin_path("rust.yaml")),
        None,
    )
    .unwrap();
    assert_eq!(
        (setup.validation_plan_for_role_fn)(Some("reviewer")),
        (setup.validation_plan_for_role_fn)(None),
        "a role with no override must fall back to the default validation plan"
    );
}
