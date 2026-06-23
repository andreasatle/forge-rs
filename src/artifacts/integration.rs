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

    // Push with --force-with-lease to guard against a race between the pre-check
    // above and the push itself. The lease requires the remote branch tip to still
    // be at the base commit; if it has since advanced, git will reject the push.
    let branch_ref = format!("{commit_sha}:refs/heads/{}", artifact.branch);
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
    if !push.status.success() {
        return Err(IntegrationError::GitCommandFailed {
            operation: "push".to_owned(),
            stderr: String::from_utf8_lossy(&push.stderr).trim().to_owned(),
        });
    }

    Ok(Artifact {
        repo_path: artifact.repo_path.clone(),
        branch: artifact.branch.clone(),
        commit_sha,
    })
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
