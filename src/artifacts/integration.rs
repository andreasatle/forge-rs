use std::process::Command;

use super::{Artifact, Workspace, artifact::assert_bare_repository};

/// Commits workspace changes and returns the resulting artifact version.
pub fn integrate(artifact: &Artifact, workspace: &Workspace) -> Artifact {
    assert_bare_repository(artifact);
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

    // A push transfers the workspace commit and advances the branch in the bare
    // artifact repository as one Git operation.
    let branch_ref = format!("{commit_sha}:refs/heads/{}", artifact.branch);
    let push = Command::new("git")
        .args(["push", "--quiet"])
        .arg(&artifact.repo_path)
        .arg(&branch_ref)
        .current_dir(workspace.path())
        .output()
        .expect("failed to run git push while integrating artifact");
    assert!(
        push.status.success(),
        "git push failed while integrating artifact: {}",
        String::from_utf8_lossy(&push.stderr).trim()
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
        .current_dir(workspace.path())
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
        .current_dir(workspace.path())
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
