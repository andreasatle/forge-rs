use super::*;
use crate::validation::ValidationTargetRule;

/// A python-like plugin: sources default to `src/{name}.py` (nested
/// under a directory), while the `tester` role runs a reduced validation
/// plan.
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
        functions: BTreeMap::new(),
        api_summary: None,
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
