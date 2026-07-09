//! Isolated `git` subprocess invocation.
//!
//! Every `git` subprocess forge-rs spawns must be built with [`command`]
//! rather than `Command::new("git")` directly. Git resolves `GIT_DIR`,
//! `GIT_WORK_TREE`, `GIT_INDEX_FILE`, and `GIT_PREFIX` from the environment
//! ahead of the process's working directory, so a process launched from
//! inside a git hook (which sets these for the hook's own repository) would
//! otherwise silently operate against the *outer* repository instead of the
//! one selected via `current_dir`.

use std::process::Command;

/// Build a `git` [`Command`] with the environment variables a git hook sets
/// (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_PREFIX`) cleared, so
/// the caller's `current_dir` always determines which repository is used.
pub(crate) fn command() -> Command {
    let mut command = Command::new("git");
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
    command
}
