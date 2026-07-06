use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::validation::ValidationScope;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let seq = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-api-summary-{label}-{}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("failed to create temp dir");
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
        .expect("failed to run git");
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn git_output(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("failed to run git");
    assert!(out.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn make_artifact_view(temp: &TempDir, files: &[(&str, &str)]) -> ArtifactView {
    let seed = temp.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--quiet", "--initial-branch=main"]);
    git(&seed, &["config", "user.name", "Test"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    for (name, content) in files {
        fs::write(seed.join(name), content).unwrap();
    }
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "--quiet", "-m", "init"]);
    let bare = temp.join("artifact.git");
    let status = Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&seed)
        .arg(&bare)
        .status()
        .expect("git clone --bare failed");
    assert!(status.success());
    let commit_sha = git_output(&bare, &["rev-parse", "HEAD"]);
    ArtifactView {
        repo_path: bare,
        commit_sha,
    }
}

fn cat_command() -> CommandSpec {
    CommandSpec {
        program: "cat".to_string(),
        args: vec![],
        when_files_present: vec![],
        scope: ValidationScope::Workspace,
    }
}

#[test]
fn summary_joins_per_file_output_with_headers() {
    // Invariant: each file's command output is labeled with its path so the
    // planner prompt can attribute a summary to its source file.
    let temp = TempDir::new("joins-output");
    let view = make_artifact_view(&temp, &[("a.txt", "content a\n"), ("b.txt", "content b\n")]);
    let files = view.list_files().unwrap();

    let summary = build_api_summary(&view, &files, &cat_command()).expect("summary must be Some");

    assert!(summary.contains("# a.txt\ncontent a"), "got: {summary}");
    assert!(summary.contains("# b.txt\ncontent b"), "got: {summary}");
}

#[test]
fn command_runs_against_a_real_checked_out_file() {
    // Invariant: the command executes in an actual temporary workspace
    // checkout, not just against in-memory git object content, since
    // real-world api_summary commands (ast parsers, grep) need a file on disk.
    let temp = TempDir::new("real-checkout");
    let view = make_artifact_view(&temp, &[("main.py", "def f():\n    pass\n")]);
    let files = view.list_files().unwrap();

    let summary = build_api_summary(&view, &files, &cat_command()).expect("summary must be Some");

    assert!(summary.contains("def f():"), "got: {summary}");
}

#[test]
fn failing_command_is_skipped_without_failing_the_whole_summary() {
    // Invariant: one file's command failure must not suppress summaries for
    // other files.
    let temp = TempDir::new("skip-failure");
    let view = make_artifact_view(&temp, &[("ok.txt", "fine\n")]);
    let files: Vec<PathBuf> = vec![PathBuf::from("missing.txt"), PathBuf::from("ok.txt")];

    let summary = build_api_summary(&view, &files, &cat_command()).expect("summary must be Some");

    assert!(!summary.contains("missing.txt"), "got: {summary}");
    assert!(summary.contains("# ok.txt\nfine"), "got: {summary}");
}

#[test]
fn command_producing_no_output_yields_no_summary() {
    // Invariant: an empty stdout is treated the same as "nothing to say" and
    // must not appear as an empty section.
    let temp = TempDir::new("empty-output");
    let view = make_artifact_view(&temp, &[("empty.txt", "")]);
    let files = view.list_files().unwrap();

    let summary = build_api_summary(&view, &files, &cat_command());

    assert_eq!(summary, None, "empty command output must yield no summary");
}
