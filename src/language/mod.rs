//! Declarative language specifications backed by YAML.
//!
//! Each language is described by a [`spec::LanguageSpec`] that declares init
//! commands, validation commands, and prompt guidance. Specs are bundled as
//! YAML files and loaded at runtime through the [`registry`].
//!
//! Adding a new language requires only a new YAML file and a match arm in
//! [`registry::language_spec`] — no trait impls, no new Rust types.

pub mod registry;
pub mod spec;

pub use registry::language_spec;
pub use spec::{LanguageInitSpec, LanguageSpec, LanguageValidationSpec};
