//! Git-backed artifact data-plane prototype.
//!
//! Artifacts identify committed state in bare repositories. Workspaces are
//! mutable non-bare clones of that state, and integration commits and pushes
//! a new immutable version.

mod artifact;
pub(crate) mod file_ops;
mod integration;
mod read;
mod workspace;

pub use artifact::{Artifact, ArtifactView};
pub use file_ops::{ArtifactError, WorkspaceFileOps};
pub use integration::{IntegrationError, integrate};
pub use read::ArtifactRead;
pub use workspace::{Workspace, WorkspaceFactory};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
