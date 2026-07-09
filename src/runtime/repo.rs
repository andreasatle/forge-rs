//! Artifact repository creation and loading.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::artifacts::{Artifact, Workspace};
use crate::config::ArtifactConfig;
use crate::language::spec::LanguageInitSpec;
use crate::validation::{CommandValidator, Validator};

static SEED_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Load the artifact at `config.repo_path`, creating a bare repo if it does not exist.
///
/// When `init_spec` is `Some` and the repo is newly created, its commands are
/// executed in a temporary workspace and committed as the sole initial
/// artifact revision. Language init produces exactly one commit rather than
/// an empty "Initial" followed by an integration commit.
pub fn load_or_create_artifact(
    config: &ArtifactConfig,
    init_spec: Option<&LanguageInitSpec>,
) -> Result<Artifact, Box<dyn Error>> {
    let repo_path = PathBuf::from(&config.repo_path);

    if !repo_path.exists() {
        match init_spec {
            Some(spec) if !spec.commands.is_empty() => {
                create_bare_repo_with_language_init(&repo_path, &config.branch, spec)?;
            }
            _ => create_bare_repo(&repo_path, &config.branch)?,
        }
    }

    let repo_path = repo_path.canonicalize()?;
    let commit_sha = git_rev_parse_branch(&repo_path, &config.branch)?;

    Ok(Artifact {
        repo_path,
        branch: config.branch.clone(),
        commit_sha,
    })
}

/// Initialize a bare repo whose first (and only) commit contains the output of
/// the language init commands.
fn create_bare_repo_with_language_init(
    path: &Path,
    branch: &str,
    init_spec: &LanguageInitSpec,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let seq = SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let workspace =
        std::env::temp_dir().join(format!("forge-lang-init-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&workspace)?;

    let result = init_workspace_and_clone_bare(&workspace, path, branch, init_spec);
    let _ = std::fs::remove_dir_all(&workspace);
    result
}

fn init_workspace_and_clone_bare(
    workspace: &Path,
    bare_path: &Path,
    branch: &str,
    init_spec: &LanguageInitSpec,
) -> Result<(), Box<dyn Error>> {
    run_git(
        workspace,
        &["init", "--quiet", &format!("--initial-branch={branch}")],
    )?;

    if !init_spec.gitignore.is_empty() {
        let content = init_spec.gitignore.join("\n") + "\n";
        std::fs::write(workspace.join(".gitignore"), &content)?;
    }

    let ws = Workspace::at_path(workspace.to_path_buf(), String::new());
    let timeout = Duration::from_secs(300);
    let validator = CommandValidator::new(init_spec.commands.to_vec(), timeout);
    let result = validator.validate(&ws);

    if !result.passed {
        return Err(format!("language initialization failed: {}", result.summary).into());
    }

    run_git(workspace, &["add", "--all"])?;
    run_git(
        workspace,
        &[
            "-c",
            "user.name=Forge",
            "-c",
            "user.email=forge@localhost",
            "commit",
            "--quiet",
            "-m",
            "Initial",
        ],
    )?;

    let clone = crate::git::command()
        .args(["clone", "--quiet", "--bare"])
        .arg(workspace)
        .arg(bare_path)
        .status()?;

    if !clone.success() {
        return Err("git clone --bare failed after language init".into());
    }

    Ok(())
}

fn create_bare_repo(path: &Path, branch: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let seq = SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = std::env::temp_dir().join(format!("forge-seed-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&seed)?;

    run_git(
        &seed,
        &["init", "--quiet", &format!("--initial-branch={branch}")],
    )?;
    run_git(&seed, &["config", "user.name", "Forge"])?;
    run_git(&seed, &["config", "user.email", "forge@localhost"])?;
    run_git(
        &seed,
        &["commit", "--allow-empty", "--quiet", "-m", "Initial"],
    )?;

    let status = crate::git::command()
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(path)
        .status()?;

    let _ = std::fs::remove_dir_all(&seed);

    if !status.success() {
        return Err("git clone --bare failed".into());
    }

    Ok(())
}

fn run_git(path: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let status = crate::git::command()
        .args(args)
        .current_dir(path)
        .status()?;
    if !status.success() {
        return Err(format!("git {} failed", args.join(" ")).into());
    }
    Ok(())
}

pub(super) fn git_rev_parse_branch(
    repo_path: &Path,
    branch: &str,
) -> Result<String, Box<dyn Error>> {
    let refspec = format!("refs/heads/{branch}");
    let output = crate::git::command()
        .args(["rev-parse", &refspec])
        .current_dir(repo_path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "branch '{branch}' not found in artifact repository at {}",
            repo_path.display()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
