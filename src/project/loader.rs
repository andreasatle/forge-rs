//! Project adapter loading — reads a [`YamlProjectAdapter`] from an explicit
//! YAML file path.
//!
//! There is no default adapter and no bootstrapping: `path` must already
//! exist and parse as a valid adapter config, or loading fails immediately.
//! Any language plugins the adapter declares are loaded alongside it — a
//! missing or invalid plugin also fails adapter loading immediately. Every
//! declared plugin's prompt sections are also composed together into the
//! adapter's plugin prompt layer, so an adapter with several plugins (e.g.
//! Python and Rust) gets guidance from all of them, not just one picked per
//! node.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use super::RolePromptConfig;
use super::YamlProjectAdapter;
use crate::language::LanguageSpec;
use crate::language::registry::load_plugin;

/// Load the [`YamlProjectAdapter`] at `path`, along with every language
/// plugin it declares in its `plugins:` list.
///
/// Plugin paths are resolved relative to `path`'s own directory, then loaded
/// and indexed by each plugin's declared [`LanguageSpec::extensions`] — see
/// [`YamlProjectAdapter::language_plugins`]. Each plugin's prompt sections
/// (see [`LanguageSpec::prompt_sections`]) are also merged, in declaration
/// order, into the adapter's plugin prompt layer — see
/// [`YamlProjectAdapter::with_plugin_prompt`].
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
    let mut plugin_specs: Vec<LanguageSpec> = Vec::new();
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
        plugin_specs.push(spec);
    }

    let adapter = adapter.with_language_plugins(language_plugins);
    if plugin_specs.is_empty() {
        Ok(adapter)
    } else {
        Ok(adapter.with_plugin_prompt(merge_prompt_sections(&plugin_specs)))
    }
}

/// Merge every declared plugin's prompt sections into one layer, joining
/// each of Identity/Context/Instructions/Constraints across plugins in
/// declaration order — mirroring how [`crate::roles::policy::render_role_prompt`]
/// joins the generic, adapter, and plugin layers within a single section.
fn merge_prompt_sections(specs: &[LanguageSpec]) -> RolePromptConfig {
    let sections: Vec<RolePromptConfig> = specs.iter().map(LanguageSpec::prompt_sections).collect();
    RolePromptConfig {
        identity: join_nonempty(sections.iter().map(|s| s.identity.as_str())),
        context: join_nonempty(sections.iter().map(|s| s.context.as_str())),
        instructions: join_nonempty(sections.iter().map(|s| s.instructions.as_str())),
        constraints: join_nonempty(sections.iter().map(|s| s.constraints.as_str())),
    }
}

fn join_nonempty<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
