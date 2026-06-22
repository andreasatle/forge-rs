use std::process::Command;

use super::{Artifact, Workspace};

/// Commits workspace changes and returns the resulting artifact version.
pub fn integrate(artifact: &Artifact, workspace: &Workspace) -> Artifact {
    run_git(workspace, &["add", "--all"]);
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
    );

    let commit_sha = git_stdout(workspace, &["rev-parse", "HEAD"]);

    // Transfer the detached workspace commit back to the artifact repository,
    // then advance the artifact's existing logical branch to that commit.
    let fetch = Command::new("git")
        .args(["fetch", "--quiet"])
        .arg(&workspace.path)
        .arg(&commit_sha)
        .current_dir(&artifact.repo_path)
        .output()
        .expect("failed to run git fetch while integrating artifact");
    assert!(
        fetch.status.success(),
        "git fetch failed while integrating artifact: {}",
        String::from_utf8_lossy(&fetch.stderr).trim()
    );

    let branch_ref = format!("refs/heads/{}", artifact.branch);
    let update_ref = Command::new("git")
        .args(["update-ref", &branch_ref, &commit_sha])
        .current_dir(&artifact.repo_path)
        .output()
        .expect("failed to run git update-ref while integrating artifact");
    assert!(
        update_ref.status.success(),
        "git update-ref failed while integrating artifact: {}",
        String::from_utf8_lossy(&update_ref.stderr).trim()
    );

    Artifact {
        repo_path: artifact.repo_path.clone(),
        branch: artifact.branch.clone(),
        commit_sha,
    }
}

fn run_git(workspace: &Workspace, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(&workspace.path)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn git_stdout(workspace: &Workspace, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(&workspace.path)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout)
        .expect("git output was not UTF-8")
        .trim()
        .to_owned()
}
