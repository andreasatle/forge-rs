use std::error::Error;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::Artifact;
use crate::git;

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
        let _ = git::command()
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

/// Creates git workspaces from an artifact version.
///
/// The factory owns the artifact-specific checkout policy and sanitized git
/// command setup used by workspace creation and cleanup.
pub struct WorkspaceFactory<'a> {
    artifact: &'a Artifact,
}

impl<'a> WorkspaceFactory<'a> {
    /// Creates a workspace factory for `artifact`.
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
        let add = git::command()
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
        let clone = git::command()
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

        let checkout = git::command()
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
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-workspace-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("failed to create temporary test directory");
            Self(path)
        }

        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(label: &str) -> (TempDirectory, Artifact) {
        let temp = TempDirectory::new(label);
        let seed_path = temp.join("seed");
        fs::create_dir(&seed_path).expect("failed to create seed repository directory");
        git(&seed_path, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed_path, &["config", "user.name", "Artifact Test"]);
        git(
            &seed_path,
            &["config", "user.email", "artifact-test@example.invalid"],
        );
        fs::write(seed_path.join("artifact.txt"), "version one\n")
            .expect("failed to write fixture file");
        git(&seed_path, &["add", "artifact.txt"]);
        git(&seed_path, &["commit", "--quiet", "-m", "Initial artifact"]);
        let repo_path = temp.join("artifact.git");
        git_clone_bare(&seed_path, &repo_path);
        let commit_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);

        (
            temp,
            Artifact {
                repo_path,
                branch: "main".to_owned(),
                commit_sha,
            },
        )
    }

    fn git_clone_bare(source: &Path, destination: &Path) {
        let status = crate::git::command()
            .args(["clone", "--quiet", "--bare"])
            .arg(source)
            .arg(destination)
            .status()
            .expect("failed to create bare test repository");
        assert!(status.success(), "git clone --bare failed");
    }

    fn git(path: &Path, args: &[&str]) {
        let status = crate::git::command()
            .args(args)
            .current_dir(path)
            .status()
            .expect("failed to execute git in test");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = crate::git::command()
            .args(args)
            .current_dir(path)
            .output()
            .expect("failed to execute git in test");
        assert!(output.status.success(), "git {} failed", args.join(" "));
        String::from_utf8(output.stdout)
            .expect("git output was not UTF-8")
            .trim()
            .to_owned()
    }

    #[test]
    fn create_workspace_from_artifact() {
        let (temp, artifact) = fixture("create-workspace");

        let workspace = WorkspaceFactory::new(&artifact).create_workspace(temp.join("workspace"));

        assert_eq!(workspace.base_commit, artifact.commit_sha);
        assert_eq!(
            git_output(&artifact.repo_path, &["rev-parse", "--is-bare-repository"]),
            "true"
        );
        assert_eq!(
            git_output(workspace.path(), &["rev-parse", "--is-bare-repository"]),
            "false"
        );
        assert_eq!(
            git_output(workspace.path(), &["rev-parse", "HEAD"]),
            artifact.commit_sha
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("artifact.txt")).unwrap(),
            "version one\n"
        );
    }

    #[test]
    fn temporary_workspace_removed_after_drop() {
        let (_temp, artifact) = fixture("temp-removed-drop");
        let workspace = WorkspaceFactory::new(&artifact)
            .create_temporary_workspace()
            .expect("failed to create temporary workspace");
        let path = workspace.path().to_path_buf();
        assert!(path.exists(), "workspace directory must exist before drop");
        drop(workspace);
        assert!(
            !path.exists(),
            "temporary workspace must be removed on drop"
        );
    }

    #[test]
    fn create_workspace_failure_returns_error() {
        let artifact = Artifact {
            repo_path: std::path::PathBuf::from("/nonexistent/path/that/does/not/exist.git"),
            branch: "main".to_string(),
            commit_sha: "0000000000000000000000000000000000000000".to_string(),
        };
        let result = WorkspaceFactory::new(&artifact).create_temporary_workspace();
        assert!(
            result.is_err(),
            "workspace creation from nonexistent repo must return an error"
        );
    }

    #[test]
    fn explicit_workspace_path_not_deleted_on_drop() {
        let (temp, artifact) = fixture("explicit-preserved");
        let workspace_path = temp.join("my-workspace");
        let workspace = WorkspaceFactory::new(&artifact).create_workspace(workspace_path.clone());
        assert!(workspace_path.exists());
        drop(workspace);
        assert!(
            workspace_path.exists(),
            "explicit-path workspace must not be deleted on drop"
        );
    }
}
