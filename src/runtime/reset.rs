use std::error::Error;
use std::path::{Path, PathBuf};

use crate::config::ForgeConfig;
use crate::runtime::project_setup::ProjectRuntimeSetup;

fn validate_reset_path(repo_path: &Path) -> Result<(), Box<dyn Error>> {
    // Canonicalize for comparisons when the path already exists.
    let canonical = if repo_path.exists() {
        repo_path.canonicalize()?
    } else {
        repo_path.to_path_buf()
    };

    // Must not be the filesystem root.
    if canonical == Path::new("/") {
        return Err("reset refused: repo_path must not be the filesystem root".into());
    }

    // Must not be the user's home directory.
    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(&home);
        let home_canonical = if home_path.exists() {
            home_path.canonicalize().unwrap_or(home_path)
        } else {
            home_path
        };
        if canonical == home_canonical {
            return Err("reset refused: repo_path must not be the home directory".into());
        }
    }

    // Must not be the current working directory.
    if let Ok(cwd) = std::env::current_dir()
        && canonical == cwd
    {
        return Err("reset refused: repo_path must not be the current working directory".into());
    }

    // Must end with .git — bare artifact repositories always carry this suffix.
    let path_str = repo_path.to_str().ok_or("repo_path is not valid UTF-8")?;
    if !path_str.ends_with(".git") {
        return Err(format!("reset refused: repo_path must end with .git, got: {path_str}").into());
    }

    Ok(())
}

/// Delete the artifact repository and recreate it with only the Initial commit.
pub fn run_reset(config: ForgeConfig) -> Result<(), Box<dyn Error>> {
    let repo_path = PathBuf::from(&config.artifact.repo_path);

    validate_reset_path(&repo_path)?;

    if repo_path.exists() {
        std::fs::remove_dir_all(&repo_path)?;
    }

    let setup = ProjectRuntimeSetup::build(Path::new(&config.adapter), config.validation.as_ref())?;
    let artifact =
        super::load_or_create_artifact(&config.artifact, setup.primary_language_init.as_ref())?;

    let short_sha = &artifact.commit_sha[..artifact.commit_sha.len().min(7)];
    println!("Reset complete. Initial commit: {short_sha}");

    Ok(())
}

#[cfg(test)]
#[path = "reset_tests.rs"]
mod tests;
