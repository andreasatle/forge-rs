//! Project adapter loading — reads a [`YamlProjectAdapter`] from an explicit
//! YAML file path.
//!
//! There is no default adapter and no bootstrapping: `path` must already
//! exist and parse as a valid adapter config, or loading fails immediately.
//! Any language plugins the adapter declares are loaded alongside it — a
//! missing or invalid plugin also fails adapter loading immediately. Plugin
//! prompt sections are not composed here: the node runner selects the one
//! plugin that applies to each node from its own target files (see
//! [`crate::language::select_plugin`]) and injects that plugin's prompt
//! sections per node at render time, rather than every declared plugin's
//! guidance applying to every node regardless of language.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use super::YamlProjectAdapter;
use crate::language::LanguageSpec;
use crate::language::registry::load_plugin;

/// Load the [`YamlProjectAdapter`] at `path`, along with every language
/// plugin it declares in its `plugins:` list.
///
/// Plugin paths are resolved relative to `path`'s own directory, then loaded
/// and indexed by each plugin's declared [`LanguageSpec::extensions`] — see
/// [`YamlProjectAdapter::language_plugins`].
pub fn load_adapter(path: &Path) -> Result<YamlProjectAdapter, Box<dyn Error>> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read adapter at {}: {e}", path.display()))?;
    let adapter = YamlProjectAdapter::from_yaml_str(&content).map_err(|e| {
        format!(
            "adapter at {} is not a valid adapter config: {e}",
            path.display()
        )
    })?;

    let base_dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let mut language_plugins: BTreeMap<String, LanguageSpec> = BTreeMap::new();
    for plugin_path in adapter.plugin_paths() {
        let resolved = resolve_relative(plugin_path, base_dir);
        let spec = load_plugin(&resolved).map_err(|e| {
            format!(
                "adapter at {} declares plugin '{plugin_path}' which failed to load: {e}",
                path.display()
            )
        })?;
        for extension in &spec.extensions {
            language_plugins.insert(extension.clone(), spec.clone());
        }
    }

    let adapter = adapter.with_language_plugins(language_plugins);
    adapter.validate_worker_content().map_err(|e| {
        format!(
            "adapter at {} declares an invalid worker role: {e}",
            path.display()
        )
    })?;

    Ok(adapter)
}

fn resolve_relative(path_str: &str, base_dir: Option<&Path>) -> PathBuf {
    let p = Path::new(path_str);
    match base_dir {
        Some(dir) if !p.is_absolute() => dir.join(p),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
