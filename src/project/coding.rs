//! Coding project adapter — software-oriented role prompt policy.

use super::ProjectAdapter;
use super::yaml::YamlProjectAdapter;
use crate::roles::RolePolicy;

/// Bundled configuration for the coding adapter: role prompts, ambient
/// context files, and validation target derivation rules.
const CODING_ADAPTER_CONFIG: &str = include_str!("coding.yaml");

fn coding_adapter() -> YamlProjectAdapter {
    YamlProjectAdapter::from_yaml_str(CODING_ADAPTER_CONFIG)
        .expect("bundled coding.yaml must be a valid ProjectAdapterConfig")
}

/// A [`ProjectAdapter`] with software-oriented role prompt policy.
///
/// Role prompts, context files, and validation target rules are loaded from
/// the bundled `coding.yaml` configuration rather than hardcoded in Rust.
pub struct CodingProjectAdapter;

impl ProjectAdapter for CodingProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        coding_adapter().role_policy()
    }

    fn context_file_names(&self) -> Vec<String> {
        coding_adapter().context_file_names()
    }

    fn required_validation_targets(&self, targets: &[String]) -> Vec<String> {
        coding_adapter().required_validation_targets(targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── required_validation_targets ────────────────────────────────────────────────

    #[test]
    fn required_validation_targets_derives_python_test() {
        // Invariant: Python source files produce test_ prefixed counterparts.
        assert_eq!(
            CodingProjectAdapter.required_validation_targets(&["main.py".to_string()]),
            vec!["test_main.py".to_string()],
        );
    }

    #[test]
    fn required_validation_targets_derives_rust_test() {
        // Invariant: Rust source files produce _test.rs suffixed counterparts.
        assert_eq!(
            CodingProjectAdapter.required_validation_targets(&["lib.rs".to_string()]),
            vec!["lib_test.rs".to_string()],
        );
    }

    #[test]
    fn required_validation_targets_derives_go_test() {
        // Invariant: Go source files produce _test.go suffixed counterparts.
        assert_eq!(
            CodingProjectAdapter.required_validation_targets(&["server.go".to_string()]),
            vec!["server_test.go".to_string()],
        );
    }

    #[test]
    fn required_validation_targets_derives_js_test() {
        // Invariant: JS/TS source files produce .test.ext counterparts.
        let cases: &[(&str, &str)] = &[
            ("util.js", "util.test.js"),
            ("component.ts", "component.test.ts"),
            ("widget.tsx", "widget.test.tsx"),
            ("app.jsx", "app.test.jsx"),
        ];
        for (source, expected) in cases {
            assert_eq!(
                CodingProjectAdapter.required_validation_targets(&[source.to_string()]),
                vec![expected.to_string()],
                "wrong test target for {source}"
            );
        }
    }

    #[test]
    fn required_validation_targets_excludes_test_files() {
        // Invariant: test files are not themselves source files requiring tests.
        for test_file in &[
            "test_main.py",
            "lib_test.rs",
            "server_test.go",
            "util.test.js",
        ] {
            let result = CodingProjectAdapter.required_validation_targets(&[test_file.to_string()]);
            assert!(
                result.is_empty(),
                "test file {test_file} must not produce additional test targets; got: {result:?}"
            );
        }
    }

    #[test]
    fn required_validation_targets_excludes_non_code_files() {
        // Invariant: non-code files (docs, config) have no test targets.
        for non_code in &["README.md", "config.yaml", "pyproject.toml", "Cargo.lock"] {
            let result = CodingProjectAdapter.required_validation_targets(&[non_code.to_string()]);
            assert!(
                result.is_empty(),
                "non-code file {non_code} must produce no test targets; got: {result:?}"
            );
        }
    }

    #[test]
    fn required_validation_targets_preserves_directory_prefix() {
        // Invariant: directory prefix is preserved in derived test path.
        assert_eq!(
            CodingProjectAdapter.required_validation_targets(&["src/main.py".to_string()]),
            vec!["src/test_main.py".to_string()],
        );
        assert_eq!(
            CodingProjectAdapter.required_validation_targets(&["pkg/server.go".to_string()]),
            vec!["pkg/server_test.go".to_string()],
        );
        assert_eq!(
            CodingProjectAdapter.required_validation_targets(&["lib/util.rs".to_string()]),
            vec!["lib/util_test.rs".to_string()],
        );
    }

    #[test]
    fn required_validation_targets_handles_multiple_sources() {
        // Invariant: each source file independently produces its test target.
        let mut result = CodingProjectAdapter
            .required_validation_targets(&["main.py".to_string(), "utils.rs".to_string()]);
        result.sort();
        let mut expected = vec!["test_main.py".to_string(), "utils_test.rs".to_string()];
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn required_validation_targets_mixed_source_and_test_files() {
        // Invariant: test files in the input are excluded; only source files get targets.
        let targets = vec![
            "main.py".to_string(),
            "test_main.py".to_string(),
            "lib.rs".to_string(),
        ];
        let mut result = CodingProjectAdapter.required_validation_targets(&targets);
        result.sort();
        let mut expected = vec!["test_main.py".to_string(), "lib_test.rs".to_string()];
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn required_validation_targets_empty_input_returns_empty() {
        // Invariant: empty input always returns empty output.
        assert!(
            CodingProjectAdapter
                .required_validation_targets(&[])
                .is_empty()
        );
    }
}
