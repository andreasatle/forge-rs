use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::Artifact;

/// A temporary mutable, non-bare checkout derived from an artifact version.
///
/// When `cleanup` is `Some`, the temporary directory is deleted automatically
/// on drop. When `cleanup` is `None`, the caller owns the directory.
#[derive(Debug)]
pub struct Workspace {
    path: PathBuf,
    _cleanup: Option<WorkspaceCleanup>,
    /// Commit from which the workspace was created.
    pub base_commit: String,
}

#[derive(Debug)]
enum WorkspaceCleanup {
    GitWorktree {
        _temp: TempDir,
        repo_path: PathBuf,
        path: PathBuf,
    },
}

impl Drop for WorkspaceCleanup {
    fn drop(&mut self) {
        let WorkspaceCleanup::GitWorktree {
            repo_path, path, ..
        } = self;
        let _ = git_command()
            .args(["worktree", "remove", "--force"])
            .arg(path)
            .current_dir(repo_path)
            .output();
    }
}

impl Workspace {
    /// Creates a workspace descriptor rooted at `path` with no automatic cleanup.
    ///
    /// The caller is responsible for the lifetime of the directory.
    pub fn at_path(path: PathBuf, base_commit: String) -> Self {
        Self {
            path,
            _cleanup: None,
            base_commit,
        }
    }

    /// Returns the root path of the workspace.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub struct WorkspaceFactory<'a> {
    artifact: &'a Artifact,
}

impl<'a> WorkspaceFactory<'a> {
    pub fn new(artifact: &'a Artifact) -> Self {
        Self { artifact }
    }

    /// Creates a mutable clone checked out at the artifact's exact commit.
    ///
    /// The directory at `workspace_path` is NOT deleted on drop. The caller
    /// is responsible for any cleanup.
    pub fn create_workspace(&self, workspace_path: PathBuf) -> Workspace {
        self.clone_artifact(&workspace_path)
            .expect("failed to create workspace via git clone/checkout");
        Workspace::at_path(workspace_path, self.artifact.commit_sha.clone())
    }

    /// Creates a detached git worktree in a freshly allocated temporary directory.
    pub fn create_temporary_workspace(&self) -> Result<Workspace, Box<dyn Error>> {
        let temp = TempDir::new()?;
        let workspace_path = temp.path().join("worktree");
        self.add_worktree(&workspace_path)?;
        Ok(Workspace {
            path: workspace_path.clone(),
            _cleanup: Some(WorkspaceCleanup::GitWorktree {
                _temp: temp,
                repo_path: self.artifact.repo_path.clone(),
                path: workspace_path,
            }),
            base_commit: self.artifact.commit_sha.clone(),
        })
    }

    fn add_worktree(&self, workspace_path: &Path) -> Result<(), Box<dyn Error>> {
        let add = Self::git_command()
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(workspace_path)
            .arg(&self.artifact.commit_sha)
            .current_dir(&self.artifact.repo_path)
            .output()
            .map_err(|e| format!("failed to run git worktree add: {e}"))?;
        if !add.status.success() {
            return Err(format!(
                "git worktree add failed while creating workspace: {}",
                String::from_utf8_lossy(&add.stderr).trim()
            )
            .into());
        }
        Ok(())
    }

    fn clone_artifact(&self, workspace_path: &Path) -> Result<(), Box<dyn Error>> {
        let clone = Self::git_command()
            .args(["clone", "--quiet", "--no-checkout"])
            .arg(&self.artifact.repo_path)
            .arg(workspace_path)
            .output()
            .map_err(|e| format!("failed to run git clone: {e}"))?;
        if !clone.status.success() {
            return Err(format!(
                "git clone failed while creating workspace: {}",
                String::from_utf8_lossy(&clone.stderr).trim()
            )
            .into());
        }

        let checkout = Self::git_command()
            .args(["checkout", "--quiet", "--detach"])
            .arg(&self.artifact.commit_sha)
            .current_dir(workspace_path)
            .output()
            .map_err(|e| format!("failed to run git checkout: {e}"))?;
        if !checkout.status.success() {
            return Err(format!(
                "git checkout failed while creating workspace: {}",
                String::from_utf8_lossy(&checkout.stderr).trim()
            )
            .into());
        }

        Ok(())
    }

    pub(crate) fn git_command() -> Command {
        let mut command = Command::new("git");
        command
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_PREFIX");
        command
    }
}

/// Creates a mutable clone checked out at the artifact's exact commit.
///
/// The directory at `workspace_path` is NOT deleted on drop. The caller
/// is responsible for any cleanup.
pub fn create_workspace(artifact: &Artifact, workspace_path: PathBuf) -> Workspace {
    WorkspaceFactory::new(artifact).create_workspace(workspace_path)
}

/// Creates a detached git worktree in a freshly allocated temporary directory.
///
/// The worktree is removed and the temporary parent directory is deleted when
/// the returned [`Workspace`] is dropped. Even if validation or integration
/// fails, the directory is removed as long as the `Workspace` value is dropped.
pub fn create_temporary_workspace(artifact: &Artifact) -> Result<Workspace, Box<dyn Error>> {
    WorkspaceFactory::new(artifact).create_temporary_workspace()
}

pub(crate) fn git_command() -> Command {
    WorkspaceFactory::git_command()
}
