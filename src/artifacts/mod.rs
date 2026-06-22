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
pub use update::{ArtifactUpdate, FileChange};
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

    fn write_update(path: &str, content: &str) -> ArtifactUpdate {
        ArtifactUpdate {
            changes: vec![FileChange::Write {
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
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        write_update("nested/artifact.txt", "replacement\n")
            .apply(&mut workspace)
            .unwrap();

        assert_eq!(
            fs::read_to_string(workspace.path.join("nested/artifact.txt")).unwrap(),
            "replacement\n"
        );
    }

    #[test]
    fn integrate_creates_new_commit() {
        let (temp, artifact) = fixture("integrate");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        write_update("artifact.txt", "version two\n")
            .apply(&mut workspace)
            .unwrap();

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
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        write_update("artifact.txt", "version two\n")
            .apply(&mut workspace)
            .unwrap();

        let integrated = integrate(&artifact, &workspace);

        assert_eq!(integrated.branch, artifact.branch);
    }

    #[test]
    fn two_integrations_produce_two_versions() {
        let (temp, first) = fixture("two-integrations");
        let mut first_workspace = create_workspace(&first, temp.join("workspace-one"));
        write_update("artifact.txt", "version two\n")
            .apply(&mut first_workspace)
            .unwrap();
        let second = integrate(&first, &first_workspace);

        let mut second_workspace = create_workspace(&second, temp.join("workspace-two"));
        write_update("artifact.txt", "version three\n")
            .apply(&mut second_workspace)
            .unwrap();
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

    #[test]
    fn apply_write_change() {
        let (temp, artifact) = fixture("apply-write");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "new.txt".to_owned(),
                content: "created\n".to_owned(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();

        assert_eq!(workspace.read_file("new.txt").unwrap(), "created\n");
    }

    #[test]
    fn apply_replace_change() {
        let (temp, artifact) = fixture("apply-replace");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        ArtifactUpdate {
            changes: vec![FileChange::Replace {
                path: "artifact.txt".to_owned(),
                old: "version one".to_owned(),
                new: "version two".to_owned(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();

        assert_eq!(
            workspace.read_file("artifact.txt").unwrap(),
            "version two\n"
        );
    }

    #[test]
    fn apply_delete_change() {
        let (temp, artifact) = fixture("apply-delete");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        ArtifactUpdate {
            changes: vec![FileChange::Delete {
                path: "artifact.txt".to_owned(),
            }],
        }
        .apply(&mut workspace)
        .unwrap();

        assert!(!workspace.path.join("artifact.txt").exists());
    }

    #[test]
    fn multiple_changes_apply_in_order() {
        let (temp, artifact) = fixture("multiple-changes");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace.write_file("bar.txt", "bar\n").unwrap();

        ArtifactUpdate {
            changes: vec![
                FileChange::Write {
                    path: "foo.txt".to_owned(),
                    content: "hello\n".to_owned(),
                },
                FileChange::Replace {
                    path: "foo.txt".to_owned(),
                    old: "hello".to_owned(),
                    new: "world".to_owned(),
                },
                FileChange::Delete {
                    path: "bar.txt".to_owned(),
                },
            ],
        }
        .apply(&mut workspace)
        .unwrap();

        assert_eq!(workspace.read_file("foo.txt").unwrap(), "world\n");
        assert!(!workspace.path.join("bar.txt").exists());
    }

    #[test]
    fn update_stops_on_first_error() {
        let (temp, artifact) = fixture("stops-on-error");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        let result = ArtifactUpdate {
            changes: vec![
                FileChange::Write {
                    path: "foo.txt".to_owned(),
                    content: "hello\n".to_owned(),
                },
                FileChange::Replace {
                    path: "foo.txt".to_owned(),
                    old: "not present".to_owned(),
                    new: "replacement".to_owned(),
                },
                FileChange::Delete {
                    path: "foo.txt".to_owned(),
                },
            ],
        }
        .apply(&mut workspace);

        assert_eq!(result, Err(ArtifactError::ReplaceTargetMissing));
        assert!(workspace.path.join("foo.txt").exists());
    }

    #[test]
    fn path_outside_workspace_propagates() {
        let (temp, artifact) = fixture("path-outside");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));

        let result = ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: "../outside.txt".to_owned(),
                content: "bad\n".to_owned(),
            }],
        }
        .apply(&mut workspace);

        assert_eq!(result, Err(ArtifactError::PathOutsideWorkspace));
    }
}
