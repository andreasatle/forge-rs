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
    fn required_validation_targets_is_empty_without_language_rules() {
        // Invariant: validation target derivation rules now live in the
        // active LanguageSpec (see src/language/*.yaml), not in coding.yaml —
        // the coding adapter's own config carries no rules of its own.
        assert!(
            CodingProjectAdapter
                .required_validation_targets(&["main.py".to_string()])
                .is_empty()
        );
    }
}
