//! Language plugin loading — resolves a plugin filename (e.g.
//! `"python.yaml"`) against a directory of language spec YAML files.
//!
//! Mirrors [`crate::project::loader::load_adapter`]: built-in plugins ship as
//! YAML seed content embedded in the binary and are written to the plugins
//! directory the first time they're requested. Any other filename already
//! present in the directory loads identically, so a new language plugin can
//! be added by dropping a YAML file in, with no Rust changes.

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use super::spec::LanguageSpec;

const BUILTIN_RUST: &str = include_str!("rust.yaml");
const BUILTIN_PYTHON: &str = include_str!("python.yaml");

#[cfg(test)]
static TEST_LANGUAGE_SPECS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, LanguageSpec>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn register_test_language_spec(id: impl Into<String>, spec: LanguageSpec) {
    TEST_LANGUAGE_SPECS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .expect("test language registry mutex poisoned")
        .insert(id.into(), spec);
}

#[cfg(test)]
fn test_override(id: &str) -> Option<LanguageSpec> {
    TEST_LANGUAGE_SPECS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .expect("test language registry mutex poisoned")
        .get(id)
        .cloned()
}

#[cfg(not(test))]
fn test_override(_id: &str) -> Option<LanguageSpec> {
    None
}

/// Bundled seed content for `filename`, or `None` if it doesn't name a
/// built-in plugin.
fn builtin_seed(filename: &str) -> Option<&'static str> {
    match filename {
        "rust.yaml" => Some(BUILTIN_RUST),
        "python.yaml" => Some(BUILTIN_PYTHON),
        _ => None,
    }
}

/// Load the [`LanguageSpec`] named `filename` from `dir`.
///
/// If `filename` does not already exist in `dir`, but names a built-in
/// plugin (`rust.yaml` or `python.yaml`), its bundled seed content is written
/// to `dir` first. Any other missing filename is a hard error.
pub fn load_plugin(dir: &Path, filename: &str) -> Result<LanguageSpec, Box<dyn Error>> {
    if let Some(spec) = test_override(filename) {
        return Ok(spec);
    }

    let path = dir.join(filename);
    if !path.exists() {
        seed_builtin(dir, filename)?;
    }

    let content = fs::read_to_string(&path).map_err(|e| {
        format!(
            "failed to read plugin '{filename}' at {}: {e}",
            path.display()
        )
    })?;
    serde_yaml::from_str(&content).map_err(|e| {
        format!(
            "plugin '{filename}' at {} is not a valid language spec: {e}",
            path.display()
        )
        .into()
    })
}

/// Write `filename`'s bundled seed content into `dir`, unless it's not a
/// recognised built-in (a hard error) or another caller has already seeded
/// it concurrently (a no-op).
fn seed_builtin(dir: &Path, filename: &str) -> Result<(), Box<dyn Error>> {
    let Some(seed) = builtin_seed(filename) else {
        return Err(format!("plugin not found: {filename}").into());
    };

    fs::create_dir_all(dir)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(filename))
    {
        Ok(mut file) => Ok(file.write_all(seed.as_bytes())?),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Return the [`LanguageSpec`] for bare language id `id` (e.g. `"rust"`),
/// loading `<id>.yaml` from the default binary-relative `plugins` directory.
///
/// A convenience for call sites (mostly tests) that want "the rust spec"
/// without naming a specific plugins directory or config-facing filename.
/// Production code that resolves a `ForgeConfig::plugin` filename should use
/// [`load_plugin`] directly against the configured plugins directory instead.
pub fn language_spec(id: &str) -> Option<LanguageSpec> {
    if let Some(spec) = test_override(id) {
        return Some(spec);
    }
    let dir = crate::services::binary_relative_dir("plugins");
    load_plugin(&dir, &format!("{id}.yaml")).ok()
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
