//! Git-backed artifact data-plane prototype.
//!
//! Artifacts identify committed state in bare repositories. Workspaces are
//! mutable non-bare clones of that state, updates replace complete file
//! contents, and integration commits and pushes a new immutable version.

mod artifact;
mod file_ops;
mod integration;
mod update;
mod workspace;

pub use artifact::{Artifact, ArtifactView};
pub use file_ops::{ArtifactError, WorkspaceFileOps};
pub use integration::integrate;
pub use update::{ArtifactUpdate, UpdatedFile, apply_update};
pub use workspace::{Workspace, create_workspace};

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

    fn replacement(path: &str, content: &str) -> ArtifactUpdate {
        ArtifactUpdate {
            files: vec![UpdatedFile {
                path: path.to_owned(),
                content: content.to_owned(),
            }],
        }
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

    #[test]
    fn create_workspace_from_artifact() {
        let (temp, artifact) = fixture("create-workspace");

        let workspace = create_workspace(&artifact, temp.join("workspace"));

        assert_eq!(workspace.base_commit, artifact.commit_sha);
        assert_eq!(
            git_output(&artifact.repo_path, &["rev-parse", "--is-bare-repository"]),
            "true"
        );
        assert_eq!(
            git_output(&workspace.path, &["rev-parse", "--is-bare-repository"]),
            "false"
        );
        assert_eq!(
            git_output(&workspace.path, &["rev-parse", "HEAD"]),
            artifact.commit_sha
        );
        assert_eq!(
            fs::read_to_string(workspace.path.join("artifact.txt")).unwrap(),
            "version one\n"
        );
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
            fs::read_to_string(workspace.path.join("nested/deeper/file.txt")).unwrap(),
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

        assert!(!workspace.path.join("artifact.txt").exists());
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
    fn apply_update_changes_file_contents() {
        let (temp, artifact) = fixture("apply-update");
        let workspace = create_workspace(&artifact, temp.join("workspace"));

        apply_update(
            &workspace,
            &replacement("nested/artifact.txt", "replacement\n"),
        );

        assert_eq!(
            fs::read_to_string(workspace.path.join("nested/artifact.txt")).unwrap(),
            "replacement\n"
        );
    }

    #[test]
    fn integrate_creates_new_commit() {
        let (temp, artifact) = fixture("integrate");
        let workspace = create_workspace(&artifact, temp.join("workspace"));
        apply_update(&workspace, &replacement("artifact.txt", "version two\n"));

        let integrated = integrate(&artifact, &workspace);

        assert_ne!(integrated.commit_sha, artifact.commit_sha);
        assert_eq!(
            git_output(&artifact.repo_path, &["rev-parse", "main"]),
            integrated.commit_sha
        );
    }

    #[test]
    fn artifact_preserves_branch() {
        let (temp, artifact) = fixture("preserve-branch");
        let workspace = create_workspace(&artifact, temp.join("workspace"));
        apply_update(&workspace, &replacement("artifact.txt", "version two\n"));

        let integrated = integrate(&artifact, &workspace);

        assert_eq!(integrated.branch, artifact.branch);
    }

    #[test]
    fn two_integrations_produce_two_versions() {
        let (temp, first) = fixture("two-integrations");
        let first_workspace = create_workspace(&first, temp.join("workspace-one"));
        apply_update(
            &first_workspace,
            &replacement("artifact.txt", "version two\n"),
        );
        let second = integrate(&first, &first_workspace);

        let second_workspace = create_workspace(&second, temp.join("workspace-two"));
        apply_update(
            &second_workspace,
            &replacement("artifact.txt", "version three\n"),
        );
        let third = integrate(&second, &second_workspace);

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
}
