//! TDD variant of the coding project adapter.
//!
//! Same validation-target derivation and context files as
//! [`super::CodingProjectAdapter`], but with role prompts that require the
//! planner to schedule test nodes before the implementation nodes they cover,
//! and require the worker to import functions under test rather than
//! reimplementing them.

use super::ProjectAdapter;
use super::yaml::YamlProjectAdapter;
use crate::roles::RolePolicy;

/// Bundled configuration for the TDD coding adapter, loaded from
/// `coding_tdd.yaml`.
const CODING_TDD_ADAPTER_CONFIG: &str = include_str!("coding_tdd.yaml");

fn coding_tdd_adapter() -> YamlProjectAdapter {
    YamlProjectAdapter::from_yaml_str(CODING_TDD_ADAPTER_CONFIG)
        .expect("bundled coding_tdd.yaml must be a valid ProjectAdapterConfig")
}

/// A [`ProjectAdapter`] with TDD-oriented role prompt policy: test nodes are
/// planned before the implementation nodes they cover, and workers import
/// from the source module under test instead of reimplementing it.
pub struct CodingTddProjectAdapter;

impl ProjectAdapter for CodingTddProjectAdapter {
    fn role_policy(&self) -> RolePolicy {
        coding_tdd_adapter().role_policy()
    }

    fn context_file_names(&self) -> Vec<String> {
        coding_tdd_adapter().context_file_names()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_producer_prompt_requires_test_nodes_before_implementation() {
        // Invariant: the TDD planner prompt must instruct the planner to
        // schedule test nodes ahead of the implementation nodes they cover.
        let policy = CodingTddProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("before the implementation nodes"),
            "TDD planner prompt must require test nodes before implementation nodes; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn planner_producer_prompt_requires_naming_source_module() {
        let policy = CodingTddProjectAdapter.role_policy();
        assert!(
            policy
                .planner_producer_system
                .contains("name the source module"),
            "TDD planner prompt must require naming the source module tests import from; got:\n{}",
            policy.planner_producer_system
        );
    }

    #[test]
    fn worker_producer_prompt_requires_importing_functions_under_test() {
        let policy = CodingTddProjectAdapter.role_policy();
        assert!(
            policy
                .worker_producer_system
                .contains("import the functions under test"),
            "TDD worker prompt must require importing functions under test; got:\n{}",
            policy.worker_producer_system
        );
    }

    #[test]
    fn context_file_names_includes_readme() {
        assert!(
            CodingTddProjectAdapter
                .context_file_names()
                .contains(&"README.md".to_string())
        );
    }
}
