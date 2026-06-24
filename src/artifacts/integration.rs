use std::fmt;
use std::process::Command;

use super::{Artifact, Workspace};

/// Errors that can occur while committing and pushing a workspace to the artifact repository.
#[derive(Debug)]
pub enum IntegrationError {
    /// A Git command exited with a non-zero status.
    GitCommandFailed {
        /// Short description of the Git operation (e.g. `"add --all"`).
        operation: String,
        /// Captured stderr output from the failed command.
        stderr: String,
    },
    /// A Git command succeeded but produced output that could not be decoded.
    InvalidGitOutput {
        /// Short description of the Git operation that produced the bad output.
        operation: String,
        /// Reason the output was rejected.
        reason: String,
    },
    /// The artifact branch advanced since the workspace was created.
    ///
    /// The workspace was based on `expected` but the branch tip is now `actual`.
    /// Integration was refused to prevent overwriting the intervening commits.
    Conflict {
        /// Branch whose tip no longer matches the workspace base.
        branch: String,
        /// Commit the workspace was based on.
        expected: String,
        /// Commit the branch tip has advanced to.
        actual: String,
    },
}

impl fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntegrationError::GitCommandFailed { operation, stderr } => {
                write!(f, "git {operation} failed: {stderr}")
            }
            IntegrationError::InvalidGitOutput { operation, reason } => {
                write!(f, "git {operation} produced invalid output: {reason}")
            }
            IntegrationError::Conflict {
                branch,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "integration conflict on branch {branch}: expected {expected}, actual {actual}"
                )
            }
        }
    }
}

impl std::error::Error for IntegrationError {}

/// Commits workspace changes into the artifact's bare repository and returns
/// the resulting artifact version.
pub fn integrate(artifact: &Artifact, workspace: &Workspace) -> Result<Artifact, IntegrationError> {
    integrate_inner(artifact, workspace, || {})
}

fn integrate_inner(
    artifact: &Artifact,
    workspace: &Workspace,
    pre_push_hook: impl FnOnce(),
) -> Result<Artifact, IntegrationError> {
    check_bare_repository(artifact)?;

    // CAS pre-check: refuse integration immediately if the branch tip has
    // advanced since the workspace was created. Checking before staging avoids
    // wasted work and produces a clear error before any local commits are made.
    let actual_tip = read_branch_tip(artifact)?;
    if actual_tip != workspace.base_commit {
        return Err(IntegrationError::Conflict {
            branch: artifact.branch.clone(),
            expected: workspace.base_commit.clone(),
            actual: actual_tip,
        });
    }

    run_git(workspace, &["add", "--all"])?;
    run_git(
        workspace,
        &[
            "-c",
            "user.name=Forge Artifact Prototype",
            "-c",
            "user.email=forge-artifacts@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Integrate artifact update",
        ],
    )?;

    let commit_sha = git_stdout(workspace, &["rev-parse", "HEAD"])?;

    // Allow tests to inject a race between the pre-check and the push.
    pre_push_hook();

    push_with_lease(artifact, workspace, &commit_sha)?;

    Ok(Artifact {
        repo_path: artifact.repo_path.clone(),
        branch: artifact.branch.clone(),
        commit_sha,
    })
}

/// Pushes `new_commit` to the artifact branch using `--force-with-lease`.
///
/// On failure, re-reads the branch tip and returns `Conflict` when the tip
/// has advanced past `workspace.base_commit`; otherwise returns the original
/// `GitCommandFailed`. This distinguishes CAS races from unrelated push errors.
fn push_with_lease(
    artifact: &Artifact,
    workspace: &Workspace,
    new_commit: &str,
) -> Result<(), IntegrationError> {
    let branch_ref = format!("{new_commit}:refs/heads/{}", artifact.branch);
    let lease_arg = format!(
        "--force-with-lease=refs/heads/{}:{}",
        artifact.branch, workspace.base_commit
    );
    let push = Command::new("git")
        .args(["push", "--quiet", &lease_arg])
        .arg(&artifact.repo_path)
        .arg(&branch_ref)
        .current_dir(workspace.path())
        .output()
        .map_err(|e| IntegrationError::GitCommandFailed {
            operation: "push".to_owned(),
            stderr: e.to_string(),
        })?;
    if push.status.success() {
        return Ok(());
    }

    let original_err = IntegrationError::GitCommandFailed {
        operation: "push".to_owned(),
        stderr: String::from_utf8_lossy(&push.stderr).trim().to_owned(),
    };

    // Re-read the branch tip to distinguish a CAS race from an unrelated failure.
    // If rev-parse itself fails, fall through and return the original push error.
    match read_branch_tip(artifact) {
        Ok(current_tip) if current_tip != workspace.base_commit => {
            Err(IntegrationError::Conflict {
                branch: artifact.branch.clone(),
                expected: workspace.base_commit.clone(),
                actual: current_tip,
            })
        }
        _ => Err(original_err),
    }
}

fn read_branch_tip(artifact: &Artifact) -> Result<String, IntegrationError> {
    let refname = format!("refs/heads/{}", artifact.branch);
    let op = format!("rev-parse {refname}");
    let output = Command::new("git")
        .args(["rev-parse", &refname])
        .current_dir(&artifact.repo_path)
        .output()
        .map_err(|e| IntegrationError::GitCommandFailed {
            operation: op.clone(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(IntegrationError::GitCommandFailed {
            operation: op,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|e| IntegrationError::InvalidGitOutput {
            operation: op,
            reason: e.to_string(),
        })
        .map(|s| s.trim().to_owned())
}

fn check_bare_repository(artifact: &Artifact) -> Result<(), IntegrationError> {
    let op = "rev-parse --is-bare-repository".to_owned();
    let output = Command::new("git")
        .args(["rev-parse", "--is-bare-repository"])
        .current_dir(&artifact.repo_path)
        .output()
        .map_err(|e| IntegrationError::GitCommandFailed {
            operation: op.clone(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(IntegrationError::GitCommandFailed {
            operation: op,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let value =
        String::from_utf8(output.stdout).map_err(|e| IntegrationError::InvalidGitOutput {
            operation: op.clone(),
            reason: e.to_string(),
        })?;
    if value.trim() != "true" {
        return Err(IntegrationError::GitCommandFailed {
            operation: op,
            stderr: "repository is not bare".to_owned(),
        });
    }
    Ok(())
}

fn run_git(workspace: &Workspace, args: &[&str]) -> Result<(), IntegrationError> {
    let op = args.join(" ");
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace.path())
        .output()
        .map_err(|e| IntegrationError::GitCommandFailed {
            operation: op.clone(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(IntegrationError::GitCommandFailed {
            operation: op,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn git_stdout(workspace: &Workspace, args: &[&str]) -> Result<String, IntegrationError> {
    let op = args.join(" ");
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace.path())
        .output()
        .map_err(|e| IntegrationError::GitCommandFailed {
            operation: op.clone(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(IntegrationError::GitCommandFailed {
            operation: op.clone(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|e| IntegrationError::InvalidGitOutput {
            operation: op,
            reason: e.to_string(),
        })
        .map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifacts::file_ops::WorkspaceFileOps;
    use crate::artifacts::{Artifact, create_workspace};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("forge-int-{label}-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create temp dir");
            Self(path)
        }

        fn join(&self, s: &str) -> PathBuf {
            self.0.join(s)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn git_rev(path: &Path, refname: &str) -> String {
        let out = Command::new("git")
            .args(["rev-parse", refname])
            .current_dir(path)
            .output()
            .expect("git rev-parse");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn fixture(label: &str) -> (TempDir, Artifact) {
        let temp = TempDir::new(label);
        let seed = temp.join("seed");
        fs::create_dir(&seed).unwrap();
        git(&seed, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.invalid"]);
        fs::write(seed.join("file.txt"), "v1\n").unwrap();
        git(&seed, &["add", "file.txt"]);
        git(&seed, &["commit", "--quiet", "-m", "init"]);
        let bare = temp.join("artifact.git");
        let status = Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .status()
            .expect("git clone --bare");
        assert!(status.success());
        let sha = git_rev(&bare, "HEAD");
        let artifact = Artifact {
            repo_path: bare,
            branch: "main".to_owned(),
            commit_sha: sha,
        };
        (temp, artifact)
    }

    fn advance_branch(bare: &Path, branch: &str) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=Advancer",
                "-c",
                "user.email=adv@example.invalid",
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                "HEAD",
                "-m",
                "external advance",
            ])
            .current_dir(bare)
            .output()
            .expect("commit-tree");
        assert!(out.status.success());
        let new_sha = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        let refname = format!("refs/heads/{branch}");
        let s = Command::new("git")
            .args(["update-ref", &refname, &new_sha])
            .current_dir(bare)
            .status()
            .expect("update-ref");
        assert!(s.success());
        new_sha
    }

    /// Window B: branch advances after pre-check passes but before push.
    /// The lease rejection must be reclassified as Conflict, not GitCommandFailed.
    #[test]
    fn push_lease_rejection_classified_as_conflict() {
        let (temp, artifact) = fixture("lease-conflict");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("file.txt", "v2\n")
            .expect("write_file");

        let base = artifact.commit_sha.clone();
        let bare = artifact.repo_path.clone();
        let branch = artifact.branch.clone();
        let cell = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let cell2 = cell.clone();
        let result = integrate_inner(&artifact, &workspace, move || {
            let sha = advance_branch(&bare, &branch);
            *cell2.lock().unwrap() = sha;
        });
        let advanced_sha = cell.lock().unwrap().clone();

        match result {
            Err(IntegrationError::Conflict {
                branch,
                expected,
                actual,
            }) => {
                assert_eq!(branch, artifact.branch);
                assert_eq!(expected, base);
                assert_eq!(actual, advanced_sha);
            }
            other => panic!("expected Conflict, got: {other:#?}"),
        }

        // Branch must remain at the externally advanced commit.
        assert_eq!(git_rev(&artifact.repo_path, "HEAD"), advanced_sha);
    }

    /// Push fails for a reason unrelated to branch advancement (remote becomes
    /// unreachable). The error must remain GitCommandFailed, not Conflict.
    #[test]
    fn push_failure_without_branch_advance_remains_git_command_failed() {
        let (temp, artifact) = fixture("push-non-advance");
        let mut workspace = create_workspace(&artifact, temp.join("workspace"));
        workspace
            .write_file("file.txt", "v2\n")
            .expect("write_file");

        let bare_path = artifact.repo_path.clone();
        let hidden = temp.join("artifact-hidden.git");

        let result = integrate_inner(&artifact, &workspace, || {
            // Move the bare repo so the push target disappears.
            // rev-parse will also fail, so the fallback _ arm returns GitCommandFailed.
            fs::rename(&bare_path, &hidden).expect("rename bare repo");
        });

        // Restore so TempDir cleanup can remove everything.
        let _ = fs::rename(&hidden, &bare_path);

        match result {
            Err(IntegrationError::GitCommandFailed { .. }) => {}
            other => panic!("expected GitCommandFailed, got: {other:#?}"),
        }
    }
}
