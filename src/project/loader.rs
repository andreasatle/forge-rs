//! Runtime adapter loading — resolves an adapter filename (e.g.
//! `"coding.yaml"`) against a directory of adapter YAML files.
//!
//! Built-in adapters ship as YAML seed content embedded in the binary. The
//! first time one is requested and isn't yet present in the adapters
//! directory, its seed content is written there so it can be inspected and
//! edited like any other adapter. Any other filename already present in the
//! directory loads identically — dropping a new YAML file in is enough to
//! define a new adapter, with no Rust changes required.

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use super::YamlProjectAdapter;

const BUILTIN_CODING: &str = include_str!("coding.yaml");
const BUILTIN_CODING_TDD: &str = include_str!("coding_tdd.yaml");

/// Bundled seed content for `filename`, or `None` if it doesn't name a
/// built-in adapter.
fn builtin_seed(filename: &str) -> Option<&'static str> {
    match filename {
        "coding.yaml" => Some(BUILTIN_CODING),
        "coding_tdd.yaml" => Some(BUILTIN_CODING_TDD),
        _ => None,
    }
}

/// Load the [`YamlProjectAdapter`] named `filename` from `dir`.
///
/// If `filename` does not already exist in `dir`, but names a built-in
/// adapter (`coding.yaml` or `coding_tdd.yaml`), its bundled seed content is
/// written to `dir` first. Any other missing filename is a hard error.
pub fn load_adapter(dir: &Path, filename: &str) -> Result<YamlProjectAdapter, Box<dyn Error>> {
    let path = dir.join(filename);
    if !path.exists() {
        seed_builtin(dir, filename)?;
    }

    let content = fs::read_to_string(&path).map_err(|e| {
        format!(
            "failed to read adapter '{filename}' at {}: {e}",
            path.display()
        )
    })?;
    YamlProjectAdapter::from_yaml_str(&content).map_err(|e| {
        format!(
            "adapter '{filename}' at {} is not a valid adapter config: {e}",
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
        return Err(format!("adapter not found: {filename}").into());
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

#[cfg(test)]
#[path = "loader_tests.rs"]
mod tests;
