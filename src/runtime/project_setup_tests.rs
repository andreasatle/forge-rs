use super::*;
use crate::config::{ProjectConfig, ProjectKind, ProjectVariant, ValidationConfig};

fn project(language: Option<&str>, variant: ProjectVariant) -> ProjectConfig {
    ProjectConfig {
        kind: ProjectKind::Coding,
        language: language.map(str::to_string),
        variant,
    }
}

fn builder<'a>(
    project: &'a ProjectConfig,
    validation: Option<&'a ValidationConfig>,
) -> ProjectRuntimeSetupBuilder<'a> {
    ProjectRuntimeSetupBuilder::new(project, validation)
}

// ── adapter selection ────────────────────────────────────────────────────

#[test]
fn runtime_selects_coding_adapter() {
    let project = project(None, ProjectVariant::Coding);
    let policy = builder(&project, None).role_policy();
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
    let project = project(None, ProjectVariant::CodingTdd);
    let policy = builder(&project, None).role_policy();
    assert!(
        policy
            .planner_producer_system
            .contains("before the implementation nodes"),
        "coding_tdd variant must select the TDD planner prompt; got:\n{}",
        policy.planner_producer_system
    );
    assert!(
        policy
            .worker_producer_system
            .contains("import the functions under test"),
        "coding_tdd variant must select the TDD worker prompt; got:\n{}",
        policy.worker_producer_system
    );
}

#[test]
fn runtime_role_policy_includes_language_guidance_when_language_set() {
    let project = project(Some("rust"), ProjectVariant::Coding);
    let policy = builder(&project, None).role_policy();
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
fn runtime_role_policy_has_no_language_guidance_when_language_unset() {
    let project = project(None, ProjectVariant::Coding);
    let policy = builder(&project, None).role_policy();
    assert_eq!(
        policy.language_guidance, None,
        "role policy must have no language guidance when no language is configured"
    );
}

#[test]
fn runtime_role_policy_includes_language_constraints_when_language_set() {
    let project = project(Some("rust"), ProjectVariant::Coding);
    let policy = builder(&project, None).role_policy();
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
fn runtime_role_policy_has_no_language_constraints_when_language_unset() {
    let project = project(None, ProjectVariant::Coding);
    let policy = builder(&project, None).role_policy();
    assert_eq!(
        policy.language_constraints, None,
        "role policy must have no language constraints when no language is configured"
    );
}

// ── make_validator ───────────────────────────────────────────────────────

#[test]
fn runtime_uses_always_pass_when_validation_absent() {
    use crate::artifacts::Workspace;

    let ws = Workspace::at_path(std::env::temp_dir(), "abc".to_string());
    let project = project(None, ProjectVariant::Coding);
    let validator = builder(&project, None).validator().unwrap();
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
    let project = project(None, ProjectVariant::Coding);
    let validator = builder(&project, Some(&config)).validator().unwrap();
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
    // We use "rust" which provides cargo commands; in a bare temp dir they will
    // fail, confirming a real CommandValidator was wired up.
    let project = project(Some("rust"), ProjectVariant::Coding);
    let validator = builder(&project, None).validator().unwrap();
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
    let project = project(None, ProjectVariant::Coding);
    let validator = builder(&project, Some(&config)).validator().unwrap();
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
        let project = project(None, ProjectVariant::Coding);
        assert_eq!(
            builder(&project, Some(&config)).project_requires_tests(),
            expected,
            "command: {command:?}"
        );
    }
}

#[test]
fn project_requires_tests_false_when_no_validation() {
    let project = project(None, ProjectVariant::Coding);
    assert!(
        !builder(&project, None).project_requires_tests(),
        "absent validation must set requires_tests = false"
    );
}

// ── ProjectRuntimeSetup::build ───────────────────────────────────────────

#[test]
fn build_derives_validator_and_validation_plan_from_language() {
    let project = ProjectConfig {
        kind: ProjectKind::Coding,
        language: Some("rust".to_string()),
        variant: ProjectVariant::Coding,
    };
    let setup = ProjectRuntimeSetup::build(&project, None).unwrap();
    assert!(
        setup.validation_plan.is_some(),
        "a configured language must produce a validation plan"
    );
    assert!(
        setup
            .role_policy
            .language_guidance
            .is_some_and(|g| !g.is_empty()),
        "build() must thread language guidance into the role policy"
    );
}
