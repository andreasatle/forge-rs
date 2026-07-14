//! Declarative language specifications backed by YAML.
//!
//! Each language is described by a [`spec::LanguageSpec`] that declares init
//! commands, validation commands, and prompt guidance. Specs are loaded at
//! runtime from a plugins directory through [`registry::load_plugin`]; a
//! handful ship as built-in seed content and are written into that directory
//! on first use.
//!
//! Adding a new language requires only a new YAML file dropped into the
//! plugins directory — no Rust changes.

pub mod registry;
pub mod spec;

use std::collections::BTreeMap;
use std::path::Path;

pub use registry::language_spec;
pub use spec::{
    LanguageInitSpec, LanguageSpec, LanguageValidationSpec, NameTargetRule, derive_target_from_name,
};

/// Picks the language plugin that applies to a node from the extensions of
/// its target files: the first target file (in order) whose extension has a
/// registered plugin wins. Returns `None` when no target file's extension
/// matches any configured plugin.
///
/// Shared by validation/test-target derivation (keyed by extension, no
/// prompt content) and per-node prompt rendering (keyed by extension, using
/// [`LanguageSpec::prompt_sections`]) — both need the same node-to-plugin
/// selection.
pub fn select_plugin<'a>(
    plugins: &'a BTreeMap<String, LanguageSpec>,
    target_files: &[String],
) -> Option<&'a LanguageSpec> {
    target_files.iter().find_map(|file| {
        let extension = Path::new(file).extension()?.to_str()?;
        plugins.get(extension)
    })
}

/// Required validation targets (e.g. test files) implied by `target_files`,
/// derived from whichever plugin in `plugins` matches by extension. Empty
/// when no plugin matches `target_files`, or the matching plugin doesn't run
/// tests.
///
/// Shared by the planner path (`ProjectRuntimeSetup`'s `required_test_targets_fn`)
/// and the `ForTasks` multi-team spawn path, so both stamp
/// `required_validation_targets` from the same plugin rules.
pub fn required_validation_targets(
    plugins: &BTreeMap<String, LanguageSpec>,
    target_files: &[String],
) -> Vec<String> {
    match select_plugin(plugins, target_files) {
        Some(spec) if spec.validation_includes_test_command() => {
            crate::validation::derive_validation_targets(
                &spec.validation.validation_targets,
                target_files,
            )
        }
        _ => vec![],
    }
}

/// Required validation targets for a `ForTasks`-spawned node, given the
/// spawning task's own bare `name` (see [`crate::artifacts::TaskRecord::name`]).
///
/// Prefers deriving from any `plugin_roles` entry that declares its own
/// `name_target_rules` (the same table [`derive_target_from_name`] applies
/// to derive that role's *own* sibling node's target file — see
/// `task_target_files` in `crate::machines::scheduler::triggers`), so the
/// two can never disagree: whatever a test-writing role's own ForTasks node
/// will target is exactly what gets reported here as required.
///
/// Falls back to the path-based [`required_validation_targets`] when the
/// matching plugin declares no such per-role override — e.g. a
/// single-worker-role team that writes its own tests alongside its own
/// source, with no sibling role to defer to.
pub fn required_validation_targets_for_task(
    plugins: &BTreeMap<String, LanguageSpec>,
    target_files: &[String],
    task_name: &str,
) -> Vec<String> {
    let Some(spec) = select_plugin(plugins, target_files) else {
        return vec![];
    };
    if !spec.validation_includes_test_command() {
        return vec![];
    }
    let from_roles: Vec<String> = spec
        .plugin_roles
        .iter()
        .filter(|role| !role.name_target_rules.is_empty())
        .filter_map(|role| derive_target_from_name(&role.name_target_rules, task_name))
        .collect();
    if !from_roles.is_empty() {
        return from_roles;
    }
    required_validation_targets(plugins, target_files)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
