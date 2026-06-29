use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::Workspace;
use super::artifact::ArtifactView;
use super::file_ops::{ArtifactError, WorkspaceFileOps, validate_relative_path};
use super::update::{ArtifactUpdate, FileChange};

/// Read-only interface for artifact file access.
///
/// Implemented by [`ArtifactView`] (committed git state) and
/// [`StagedArtifactView`] (committed state plus in-memory pending changes).
/// Object-safe: suitable for `Box<dyn ArtifactRead>`.
pub trait ArtifactRead {
    /// Reads a file's contents by path relative to the artifact root.
    fn read_file(&self, path: &str) -> Result<String, ArtifactError>;
    /// Lists all files, returning paths relative to the artifact root, sorted.
    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError>;
}

/// Blanket impl so that `Box<dyn ArtifactRead>` itself satisfies `ArtifactRead`.
/// This enables passing a boxed view to functions that accept `impl ArtifactRead + 'static`.
impl<T: ArtifactRead + ?Sized> ArtifactRead for Box<T> {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        (**self).read_file(path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        (**self).list_files()
    }
}

/// Delegates to the inherent `ArtifactView` methods.
impl ArtifactRead for ArtifactView {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        ArtifactView::read_file(self, path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        ArtifactView::list_files(self)
    }
}

/// Read committed files from an attempt workspace.
impl ArtifactRead for Rc<RefCell<Workspace>> {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        self.borrow().read_file(path)
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        Ok(self.borrow().list_files())
    }
}

/// A file entry in the staged layer.
#[derive(Clone, Debug)]
pub enum StagedEntry {
    /// The file was created or overwritten with this content.
    Written(String),
    /// The file was deleted.
    Deleted,
}

/// A read-only view over a committed [`ArtifactView`] with in-memory pending
/// changes layered on top.
///
/// Created by [`StagedArtifactView::from_update`] to replay an
/// [`ArtifactUpdate`] in order, mirroring the behaviour of
/// `FileToolExecutor`'s overlay so that Critic and Referee roles can read
/// files written by the Producer before those changes are committed to git.
pub struct StagedArtifactView {
    base: ArtifactView,
    staged: HashMap<PathBuf, StagedEntry>,
}

impl StagedArtifactView {
    /// Builds a staged view by replaying every change in `update` in order
    /// over `base`.
    ///
    /// For `Replace` changes the staged layer is consulted first (matching
    /// `FileToolExecutor`'s overlay semantics), so a `Write` followed by a
    /// `Replace` on the same path resolves correctly.
    ///
    /// Returns an error if a `Replace` target is missing or ambiguous.
    pub fn from_update(base: ArtifactView, update: &ArtifactUpdate) -> Result<Self, ArtifactError> {
        let mut staged: HashMap<PathBuf, StagedEntry> = HashMap::new();

        for change in &update.changes {
            match change {
                FileChange::Write { path, content } => {
                    staged.insert(PathBuf::from(path), StagedEntry::Written(content.clone()));
                }
                FileChange::Delete { path } => {
                    staged.insert(PathBuf::from(path), StagedEntry::Deleted);
                }
                FileChange::Replace { path, old, new } => {
                    let key = PathBuf::from(path);
                    let current = match staged.get(&key) {
                        Some(StagedEntry::Written(c)) => c.clone(),
                        Some(StagedEntry::Deleted) => return Err(ArtifactError::FileNotFound),
                        None => ArtifactView::read_file(&base, path)?,
                    };
                    let mut occurrences = current.match_indices(old.as_str());
                    let Some((start, _)) = occurrences.next() else {
                        return Err(ArtifactError::ReplaceTargetMissing);
                    };
                    if occurrences.next().is_some() {
                        return Err(ArtifactError::ReplaceTargetAmbiguous);
                    }
                    let mut updated = String::with_capacity(current.len() - old.len() + new.len());
                    updated.push_str(&current[..start]);
                    updated.push_str(new);
                    updated.push_str(&current[start + old.len()..]);
                    staged.insert(key, StagedEntry::Written(updated));
                }
            }
        }

        Ok(Self { base, staged })
    }
}

impl ArtifactRead for StagedArtifactView {
    fn read_file(&self, path: &str) -> Result<String, ArtifactError> {
        validate_relative_path(path)?;
        match self.staged.get(Path::new(path)) {
            Some(StagedEntry::Written(content)) => Ok(content.clone()),
            Some(StagedEntry::Deleted) => Err(ArtifactError::FileNotFound),
            None => ArtifactView::read_file(&self.base, path),
        }
    }

    fn list_files(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        let mut paths = ArtifactView::list_files(&self.base)?;
        for (staged_path, entry) in &self.staged {
            match entry {
                StagedEntry::Written(_) => {
                    if !paths.contains(staged_path) {
                        paths.push(staged_path.clone());
                    }
                }
                StagedEntry::Deleted => paths.retain(|p| p != staged_path),
            }
        }
        paths.sort();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::artifacts::{ArtifactUpdate, ArtifactView, FileChange};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("forge-staged-{label}-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &PathBuf, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn git_output(dir: &PathBuf, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("failed to run git");
        assert!(out.status.success(), "git {} failed", args.join(" "));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Creates a bare repo containing `hello.txt = "hello world\n"` and returns
    /// an `ArtifactView` pointing at that commit.
    fn make_view(label: &str) -> (TempDir, ArtifactView) {
        let temp = TempDir::new(label);

        let seed = temp.0.join("seed");
        fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "--quiet", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Staged Test"]);
        git(
            &seed,
            &["config", "user.email", "staged-test@example.invalid"],
        );
        fs::write(seed.join("hello.txt"), "hello world\n").unwrap();
        git(&seed, &["add", "hello.txt"]);
        git(&seed, &["commit", "--quiet", "-m", "init"]);

        let bare = temp.0.join("bare.git");
        Command::new("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&seed)
            .arg(&bare)
            .status()
            .expect("failed to clone bare repo");

        let sha = git_output(&bare, &["rev-parse", "HEAD"]);
        let view = ArtifactView {
            repo_path: bare,
            commit_sha: sha,
        };
        (temp, view)
    }

    fn write_update(path: &str, content: &str) -> ArtifactUpdate {
        ArtifactUpdate {
            changes: vec![FileChange::Write {
                path: path.to_owned(),
                content: content.to_owned(),
            }],
        }
    }

    // ── 1. read_file sees a file written by the Producer ─────────────────────

    #[test]
    fn staged_read_file_sees_written_file() {
        let (_temp, view) = make_view("read-written");
        let update = write_update("new.txt", "producer output\n");
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        assert_eq!(
            staged.read_file("new.txt").unwrap(),
            "producer output\n",
            "staged view must return content written by Producer"
        );
    }

    // ── 2. list_files includes Producer-written file ──────────────────────────

    #[test]
    fn staged_list_files_includes_written_file() {
        let (_temp, view) = make_view("list-written");
        let update = write_update("new.txt", "content\n");
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        let files = staged.list_files().unwrap();
        assert!(
            files.contains(&PathBuf::from("new.txt")),
            "staged list must include written file; got {files:?}"
        );
        assert!(
            files.contains(&PathBuf::from("hello.txt")),
            "staged list must retain committed files; got {files:?}"
        );
    }

    // ── 3. read_file on a deleted committed file returns not found ────────────

    #[test]
    fn staged_read_deleted_file_returns_not_found() {
        let (_temp, view) = make_view("read-deleted");
        let update = ArtifactUpdate {
            changes: vec![FileChange::Delete {
                path: "hello.txt".to_owned(),
            }],
        };
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        assert_eq!(
            staged.read_file("hello.txt"),
            Err(ArtifactError::FileNotFound),
            "staged view must hide deleted committed file"
        );
    }

    // ── 4. list_files excludes a deleted committed file ───────────────────────

    #[test]
    fn staged_list_files_excludes_deleted_file() {
        let (_temp, view) = make_view("list-deleted");
        let update = ArtifactUpdate {
            changes: vec![FileChange::Delete {
                path: "hello.txt".to_owned(),
            }],
        };
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        let files = staged.list_files().unwrap();
        assert!(
            !files.contains(&PathBuf::from("hello.txt")),
            "staged list must exclude deleted file; got {files:?}"
        );
        assert!(
            files.is_empty(),
            "list must be empty after deleting only file; got {files:?}"
        );
    }

    // ── 5. read_file returns content after replaying a Replace change ─────────

    #[test]
    fn staged_read_replayed_replace() {
        let (_temp, view) = make_view("read-replace");
        let update = ArtifactUpdate {
            changes: vec![FileChange::Replace {
                path: "hello.txt".to_owned(),
                old: "hello world".to_owned(),
                new: "goodbye".to_owned(),
            }],
        };
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        assert_eq!(
            staged.read_file("hello.txt").unwrap(),
            "goodbye\n",
            "staged view must return post-replace content"
        );
    }

    // ── 6. read_file falls back to committed content for unmodified files ─────

    #[test]
    fn staged_falls_back_to_base_for_unmodified_file() {
        let (_temp, view) = make_view("read-base-fallback");
        let update = write_update("other.txt", "other\n");
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        assert_eq!(
            staged.read_file("hello.txt").unwrap(),
            "hello world\n",
            "staged view must fall back to committed content for unmodified files"
        );
    }

    // ── 7. empty update leaves list unchanged ─────────────────────────────────

    #[test]
    fn staged_empty_update_list_matches_base() {
        let (_temp, view) = make_view("list-empty-update");
        let base_list = view.list_files().unwrap();
        let staged = StagedArtifactView::from_update(view, &ArtifactUpdate::default()).unwrap();

        assert_eq!(
            staged.list_files().unwrap(),
            base_list,
            "empty update must leave file list unchanged"
        );
    }

    // ── 8. Replace resolves against a file previously written in the update ───

    #[test]
    fn staged_replace_on_previously_written_file() {
        let (_temp, view) = make_view("replace-after-write");
        let update = ArtifactUpdate {
            changes: vec![
                FileChange::Write {
                    path: "src.txt".to_owned(),
                    content: "fn foo() {}".to_owned(),
                },
                FileChange::Replace {
                    path: "src.txt".to_owned(),
                    old: "foo".to_owned(),
                    new: "bar".to_owned(),
                },
            ],
        };
        let staged = StagedArtifactView::from_update(view, &update).unwrap();

        assert_eq!(
            staged.read_file("src.txt").unwrap(),
            "fn bar() {}",
            "staged Replace must resolve against the staged Write content"
        );
    }
}
