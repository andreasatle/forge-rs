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
pub use spec::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};

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

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
