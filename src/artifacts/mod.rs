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
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-artifacts-{label}-{}-{sequence}",
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
        let status = Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(source)
            .arg(destination)
            .status()
            .expect("failed to create bare test repository");
        assert!(status.success(), "git clone --bare failed");
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("failed to execute git in test");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
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

    fn create_workspace(artifact: &Artifact, workspace_path: PathBuf) -> Workspace {
        WorkspaceFactory::new(artifact).create_workspace(workspace_path)
    }

    #[test]
    fn read_file_returns_contents() {
        let (temp, artifact) = fixture("read-file");
        let workspace = create_workspace(&artifact, temp.join("workspace"));

        assert_eq!(
            workspace.read_file("artifact.txt").unwrap(),
            "version one\n"
        );
    }

    #[test]
    fn write_file_creates_directories() {
        let (temp, artifact) = fixture("write-file");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        workspace
            .write_file("nested/deeper/file.txt", "new contents\n")
            .unwrap();

        assert_eq!(
            fs::read_to_string(workspace.path().join("nested/deeper/file.txt")).unwrap(),
            "new contents\n"
        );
    }

    #[test]
    fn replace_text_updates_file() {
        let (temp, artifact) = fixture("replace-text");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        workspace
            .replace_text("artifact.txt", "version one", "version two")
            .unwrap();

        assert_eq!(
            workspace.read_file("artifact.txt").unwrap(),
            "version two\n"
        );
    }

    #[test]
    fn replace_text_missing_target_fails() {
        let (temp, artifact) = fixture("replace-missing");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        let result = workspace.replace_text("artifact.txt", "not present", "replacement");

        assert_eq!(result, Err(ArtifactError::ReplaceTargetMissing));
    }

    #[test]
    fn replace_text_ambiguous_target_fails() {
        let (temp, artifact) = fixture("replace-ambiguous");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("artifact.txt", "repeat repeat\n")
            .unwrap();

        let result = workspace.replace_text("artifact.txt", "repeat", "replacement");

        assert_eq!(result, Err(ArtifactError::ReplaceTargetAmbiguous));
    }

    #[test]
    fn delete_file_removes_file() {
        let (temp, artifact) = fixture("delete-file");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        workspace.delete_file("artifact.txt").unwrap();

        assert!(!workspace.path().join("artifact.txt").exists());
    }

    #[test]
    fn delete_missing_file_fails() {
        let (temp, artifact) = fixture("delete-missing");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        let result = workspace.delete_file("missing.txt");

        assert_eq!(result, Err(ArtifactError::FileNotFound));
    }

    #[test]
    fn list_files_returns_relative_paths() {
        let (temp, artifact) = fixture("list-files");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace.write_file("nested/file.txt", "nested\n").unwrap();

        assert_eq!(
            workspace.list_files(),
            vec![
                PathBuf::from("artifact.txt"),
                PathBuf::from("nested/file.txt")
            ]
        );
    }

    #[test]
    fn artifact_preserves_branch() {
        let (temp, artifact) = fixture("preserve-branch");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("artifact.txt", "version two\n")
            .unwrap();

        let integrated = integrate(&artifact, &workspace).unwrap();

        assert_eq!(integrated.branch, artifact.branch);
    }

    #[test]
    fn two_integrations_produce_two_versions() {
        let (temp, first) = fixture("two-integrations");
        let mut first_workspace = create_workspace(&first, temp.join("workspace-one"));
        first_workspace
            .write_file("artifact.txt", "version two\n")
            .unwrap();
        let second = integrate(&first, &first_workspace).unwrap();

        let mut second_workspace = create_workspace(&second, temp.join("workspace-two"));
        second_workspace
            .write_file("artifact.txt", "version three\n")
            .unwrap();
        let third = integrate(&second, &second_workspace).unwrap();

        assert_ne!(first.commit_sha, second.commit_sha);
        assert_ne!(second.commit_sha, third.commit_sha);
        assert_eq!(first.branch, second.branch);
        assert_eq!(second.branch, third.branch);
        assert_eq!(
            git_output(
                &third.repo_path,
                &["rev-parse", &format!("{}^", third.commit_sha)]
            ),
            second.commit_sha
        );
    }

    #[test]
    fn integrate_uses_existing_workspace_changes() {
        let (temp, artifact) = fixture("existing-workspace-changes");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        workspace
            .write_file("artifact.txt", "written directly\n")
            .unwrap();

        let integrated = integrate(&artifact, &workspace).unwrap();

        assert_ne!(integrated.commit_sha, artifact.commit_sha);
        assert_eq!(
            git_output(&integrated.repo_path, &["rev-parse", "main"]),
            integrated.commit_sha
        );
        let content = git_output(
            &integrated.repo_path,
            &["show", &format!("{}:artifact.txt", integrated.commit_sha)],
        );
        assert_eq!(content, "written directly");
    }

    #[test]
    fn artifact_view_reads_committed_file() {
        let (_temp, artifact) = fixture("view-reads-committed");
        let view = ArtifactView {
            repo_path: artifact.repo_path.clone(),
            commit_sha: artifact.commit_sha.clone(),
        };

        assert_eq!(view.read_file("artifact.txt").unwrap(), "version one\n");
    }

    #[test]
    fn artifact_view_does_not_see_unintegrated_workspace_changes() {
        let (temp, artifact) = fixture("view-no-unintegrated");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("artifact.txt", "modified in workspace\n")
            .unwrap();
        let view = ArtifactView {
            repo_path: artifact.repo_path.clone(),
            commit_sha: artifact.commit_sha.clone(),
        };

        assert_eq!(view.read_file("artifact.txt").unwrap(), "version one\n");
    }

    #[test]
    fn artifact_view_sees_new_commit_after_integration() {
        let (temp, artifact) = fixture("view-after-integration");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("artifact.txt", "version two\n")
            .unwrap();
        let integrated = integrate(&artifact, &workspace).unwrap();
        let view = ArtifactView {
            repo_path: integrated.repo_path.clone(),
            commit_sha: integrated.commit_sha.clone(),
        };

        assert_eq!(view.read_file("artifact.txt").unwrap(), "version two\n");
    }

    #[test]
    fn artifact_view_lists_files() {
        let (temp, artifact) = fixture("view-list-files");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace.write_file("nested/file.txt", "nested\n").unwrap();
        let integrated = integrate(&artifact, &workspace).unwrap();
        let view = ArtifactView {
            repo_path: integrated.repo_path.clone(),
            commit_sha: integrated.commit_sha.clone(),
        };

        assert_eq!(
            view.list_files().unwrap(),
            vec![
                PathBuf::from("artifact.txt"),
                PathBuf::from("nested/file.txt"),
            ]
        );
    }

    #[test]
    fn artifact_view_rejects_parent_traversal() {
        let (_temp, artifact) = fixture("view-parent-traversal");
        let view = ArtifactView {
            repo_path: artifact.repo_path.clone(),
            commit_sha: artifact.commit_sha.clone(),
        };

        for path in ["../secret", "/etc/passwd"] {
            assert_eq!(
                view.read_file(path),
                Err(ArtifactError::PathOutsideWorkspace),
                "path {path:?} must be rejected as outside the workspace",
            );
        }
    }

    #[test]
    fn integrate_returns_error_for_invalid_artifact_repo() {
        let (temp, good_artifact) = fixture("integrate-error-bad-repo");
        let workspace = create_workspace(&good_artifact, temp.join("workspace"));

        let bad_artifact = Artifact {
            repo_path: std::path::PathBuf::from("/nonexistent/path/that/does/not/exist.git"),
            branch: "main".to_owned(),
            commit_sha: good_artifact.commit_sha.clone(),
        };

        let result = integrate(&bad_artifact, &workspace);

        assert!(
            result.is_err(),
            "integrate must return Err for a nonexistent repo_path; got Ok"
        );
    }

    /// Advance the branch in a bare repo to a new commit without touching any
    /// external clone. Uses `git commit-tree` + `git update-ref` so the test
    /// does not need a second checkout.
    fn advance_branch_in_bare(bare_repo: &std::path::Path, branch: &str) -> String {
        let new_sha_out = Command::new("git")
            .args([
                "-c",
                "user.name=External Advancer",
                "-c",
                "user.email=advance@example.invalid",
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                "HEAD",
                "-m",
                "External advance",
            ])
            .current_dir(bare_repo)
            .output()
            .expect("git commit-tree failed");
        assert!(
            new_sha_out.status.success(),
            "git commit-tree must succeed in test"
        );
        let new_sha = String::from_utf8(new_sha_out.stdout)
            .expect("commit-tree output must be UTF-8")
            .trim()
            .to_owned();

        let refname = format!("refs/heads/{branch}");
        let status = Command::new("git")
            .args(["update-ref", &refname, &new_sha])
            .current_dir(bare_repo)
            .status()
            .expect("git update-ref failed");
        assert!(status.success(), "git update-ref must succeed in test");

        new_sha
    }

    #[test]
    fn integrate_conflict_if_branch_advanced_since_workspace_base() {
        let (temp, artifact) = fixture("cas-conflict");
        let workspace = create_workspace(&artifact, temp.join("workspace"));

        // Advance the branch externally after the workspace was created.
        let advanced_sha = advance_branch_in_bare(&artifact.repo_path, &artifact.branch);

        // Attempt to integrate the stale workspace.
        let result = integrate(&artifact, &workspace);

        match result {
            Err(IntegrationError::Conflict {
                branch,
                expected,
                actual,
            }) => {
                assert_eq!(branch, artifact.branch);
                assert_eq!(expected, artifact.commit_sha);
                assert_eq!(actual, advanced_sha);
            }
            other => panic!("expected IntegrationError::Conflict, got: {other:#?}"),
        }

        // Branch must remain at the externally advanced commit.
        let tip = git_output(&artifact.repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            tip, advanced_sha,
            "branch must remain at the advanced commit after conflict"
        );
    }

    #[test]
    fn integrate_succeeds_when_branch_still_at_workspace_base() {
        let (temp, artifact) = fixture("cas-succeed");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("artifact.txt", "cas version\n")
            .unwrap();

        let result = integrate(&artifact, &workspace);

        assert!(
            result.is_ok(),
            "integrate must succeed when branch tip matches workspace base; got: {result:#?}"
        );
        let new_sha = result.unwrap().commit_sha;
        assert_ne!(new_sha, artifact.commit_sha, "commit must advance");
        let tip = git_output(&artifact.repo_path, &["rev-parse", "HEAD"]);
        assert_eq!(tip, new_sha, "branch must point at the new commit");
    }
}
