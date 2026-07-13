use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::Workspace;
use super::artifact::ArtifactView;
use super::file_ops::{ArtifactError, WorkspaceFileOps};

/// Read-only interface for artifact file access.
///
/// Implemented by committed artifact views and WorkAttempt workspaces.
/// Object-safe: suitable for `Box<dyn ArtifactRead>`.
pub trait ArtifactRead {
    /// Reads a file's contents by path relative to the artifact root.
    fn read_file(&self, path: &str) -> Result<String, ArtifactError>;
    /// Lists all files, returning paths relative to the artifact root, sorted.
    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError>;
}

impl<T: ArtifactRead + ?Sized> ArtifactRead for Box<T> {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        (**self).read_file(path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        (**self).list_files()
    }
}

impl ArtifactRead for ArtifactView {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        ArtifactView::read_file(self, path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        ArtifactView::list_files(self)
    }
}

impl ArtifactRead for Arc<Mutex<Workspace>> {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        self.lock()
            .expect("workspace mutex poisoned")
            .read_file(path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        Ok(self.lock().expect("workspace mutex poisoned").list_files())
    }
}
