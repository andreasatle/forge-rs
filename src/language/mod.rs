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

pub use registry::language_spec;
pub use spec::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};
