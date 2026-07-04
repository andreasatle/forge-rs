//! Project adapter loading — reads a [`YamlProjectAdapter`] from an explicit
//! YAML file path.
//!
//! There is no default adapter and no bootstrapping: `path` must already
//! exist and parse as a valid adapter config, or loading fails immediately.

use std::error::Error;
use std::fs;
use std::path::Path;

use super::YamlProjectAdapter;

/// Load the [`YamlProjectAdapter`] at `path`.
pub fn load_adapter(path: &Path) -> Result<YamlProjectAdapter, Box<dyn Error>> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed to read adapter at {}: {e}", path.display()))?;
    YamlProjectAdapter::from_yaml_str(&content).map_err(|e| {
        format!(
            "adapter at {} is not a valid adapter config: {e}",
            path.display()
        )
        .into()
    })
}

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
