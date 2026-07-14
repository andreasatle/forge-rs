use super::*;
use crate::language::spec::LanguageRoleConfig;
use crate::validation::ValidationTargetRule;

/// A python-like plugin: sources default to `src/{name}.py` (nested
/// under a directory), while the `tester` role writes to a flat
/// `tests/test_{name}.py` — reproducing the exact shape that produced
/// mismatched sibling targets in practice (see the regression test in
/// `crate::machines::scheduler::triggers_tests`).
fn nested_source_plugin_with_flat_tester() -> LanguageSpec {
    LanguageSpec {
        extensions: vec!["py".to_string()],
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
            validation_targets: vec![ValidationTargetRule {
                pattern: "{stem}.py".to_string(),
                target: "tests/test_{stem}.py".to_string(),
            }],
        },
        plugin_roles: vec![LanguageRoleConfig {
            plugin_role: "tester".to_string(),
            validation: LanguageValidationSpec {
                runs_tests: false,
                commands: vec![],
                validation_targets: vec![],
            },
            name_target_rules: vec![NameTargetRule {
                pattern: "{name}".to_string(),
                target: "tests/test_{name}.py".to_string(),
            }],
        }],
        api_summary: None,
        name_target_rules: vec![NameTargetRule {
            pattern: "{name}".to_string(),
            target: "src/{name}.py".to_string(),
        }],
    }
}

#[test]
fn path_based_required_validation_targets_nests_under_source_directory() {
    // Invariant (documented for posterity, not a desired outcome): the
    // path-based derivation unconditionally nests a directory-qualified
    // validation target under the source's own directory. This is
    // exactly the mismatch the sibling-path audit found — asserted here
    // so a future change to this behavior is deliberate, not silent.
    let plugins = BTreeMap::from([("py".to_string(), nested_source_plugin_with_flat_tester())]);
    let targets = required_validation_targets(&plugins, &["src/fibonacci.py".to_string()]);
    assert_eq!(
        targets,
        vec!["src/tests/test_fibonacci.py".to_string()],
        "path-based derivation nests under the source's own directory prefix"
    );
}

#[test]
fn required_validation_targets_for_task_matches_the_tester_roles_own_target() {
    // Invariant: for a plugin whose tester role declares its own
    // name_target_rules, the ForTasks-path derivation must agree with
    // exactly what that role's own sibling node would target -- not the
    // path-based derivation, which (per the test above) disagrees once
    // the source is nested under a directory.
    let plugins = BTreeMap::from([("py".to_string(), nested_source_plugin_with_flat_tester())]);
    let target_files = vec!["src/fibonacci.py".to_string()];
    let targets = required_validation_targets_for_task(&plugins, &target_files, "fibonacci");
    assert_eq!(
        targets,
        vec!["tests/test_fibonacci.py".to_string()],
        "must match the tester role's own name_target_rules output, not the nested path-based one"
    );
}

#[test]
fn required_validation_targets_for_task_falls_back_to_path_based_without_a_role_override() {
    // Invariant: a plugin with no plugin_roles override (a single-role
    // team that writes its own tests) keeps the existing path-based
    // behavior -- this must not regress `for_tasks_spawns_node_with_
    // required_validation_targets_from_team_language_plugins` in
    // triggers_tests.rs.
    let spec = LanguageSpec {
        extensions: vec!["rs".to_string()],
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
            validation_targets: vec![ValidationTargetRule {
                pattern: "{stem}.rs".to_string(),
                target: "{stem}_test.rs".to_string(),
            }],
        },
        plugin_roles: vec![],
        api_summary: None,
        name_target_rules: vec![],
    };
    let plugins = BTreeMap::from([("rs".to_string(), spec)]);
    let target_files = vec!["src/fibonacci.rs".to_string()];
    let targets = required_validation_targets_for_task(&plugins, &target_files, "fibonacci");
    assert_eq!(targets, vec!["src/fibonacci_test.rs".to_string()]);
}
