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
        }
    }
}

impl std::error::Error for IntegrationError {}

/// Commits workspace changes into the artifact's bare repository and returns
/// the resulting artifact version.
pub fn integrate(artifact: &Artifact, workspace: &Workspace) -> Result<Artifact, IntegrationError> {
    check_bare_repository(artifact)?;
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

    // A push transfers the workspace commit and advances the branch in the bare
    // artifact repository as one Git operation.
    let branch_ref = format!("{commit_sha}:refs/heads/{}", artifact.branch);
    let push = Command::new("git")
        .args(["push", "--quiet"])
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
