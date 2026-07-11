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
