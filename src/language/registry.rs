//! Language plugin loading — reads a [`LanguageSpec`] from an explicit YAML
//! file path.
//!
//! There is no default plugin and no bootstrapping: `path` must already
//! exist and parse as a valid language spec, or loading fails immediately.

use std::error::Error;
use std::fs;
use std::path::Path;

use super::spec::LanguageSpec;

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

/// Load the [`LanguageSpec`] at `path`.
pub fn load_plugin(path: &Path) -> Result<LanguageSpec, Box<dyn Error>> {
    if let Some(spec) = test_override(&path.to_string_lossy()) {
        return Ok(spec);
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read plugin at {}: {e}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|e| {
        format!(
            "plugin at {} is not a valid language spec: {e}",
            path.display()
        )
        .into()
    })
}

/// Return the [`LanguageSpec`] for bare language id `id` (e.g. `"rust"`),
/// loading `<id>.yaml` from this crate's bundled `plugins/` directory.
///
/// A convenience for call sites (mostly tests) that want "the rust spec"
/// without naming a specific plugin file. Production code that resolves a
/// `ForgeConfig::plugin` path should use [`load_plugin`] directly instead.
pub fn language_spec(id: &str) -> Option<LanguageSpec> {
    if let Some(spec) = test_override(id) {
        return Some(spec);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(format!("{id}.yaml"));
    load_plugin(&path).ok()
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
