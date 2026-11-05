//! Which linked worktree a path belongs to.

use std::path::{Path, PathBuf};

use crate::git::{Git, trimmed_path};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::paths::{self, cannot_read};

/// A path resolved to a linked worktree. The main checkout cannot be one, so a
/// value of this type is proof that it is not about to be removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedWorktree {
    /// Its top level.
    pub path: PathBuf,
    /// Its private git directory, `<common>/worktrees/<id>`.
    pub gitdir: PathBuf,
    /// The repository's shared git directory.
    pub common: PathBuf,
    /// The directory holding `common`.
    pub main_checkout: PathBuf,
}

impl LinkedWorktree {
    pub fn resolve(path: &Path) -> Outcome<Self> {
        if !paths::is_dir(path)? {
            return Err(Stop::refused(format!(
                "{} is not a directory",
                path.display()
            )));
        }
        let start = path
            .canonicalize()
            .map_err(|error| cannot_read(path, &error))?;
        let question = |args: &[&str]| -> Result<PathBuf, CannotTell> {
            Git::at(&start, args).probe(&start).map(trimmed_path)
        };
        let top = question(&["rev-parse", "--show-toplevel"])?;
        let gitdir = question(&["rev-parse", "--absolute-git-dir"])?;
        let common = question(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        if gitdir == common {
            return Err(Stop::refused(format!(
                "{} is the main worktree, not a linked one",
                top.display()
            )));
        }
        let main_checkout = parent_of(&common)?;
        Ok(Self {
            path: top,
            gitdir,
            common,
            main_checkout,
        })
    }
}

/// The directory holding a repository's common git directory.
pub fn parent_of(common: &Path) -> Result<PathBuf, CannotTell> {
    common.parent().map(Path::to_path_buf).ok_or_else(|| {
        CannotTell::new(format!(
            "worktree: {} has no parent directory",
            common.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, git};

    fn repository(scratch: &Scratch) -> PathBuf {
        let repo = scratch.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        repo.canonicalize().unwrap()
    }

    #[test]
    fn a_linked_worktree_resolves_from_a_subdirectory_to_its_four_paths() {
        let scratch = Scratch::new();
        let repo = repository(&scratch);
        let wt = repo.parent().unwrap().join("wt");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "x", wt.to_str().unwrap()],
        );
        let below = wt.join("below");
        std::fs::create_dir(&below).unwrap();

        let found = LinkedWorktree::resolve(&below).unwrap();

        assert_eq!(found.path, wt);
        assert_eq!(found.gitdir, repo.join(".git/worktrees/wt"));
        assert_eq!(found.common, repo.join(".git"));
        assert_eq!(found.main_checkout, repo);
    }

    #[test]
    fn the_main_checkout_is_refused_as_a_linked_worktree() {
        let scratch = Scratch::new();
        let repo = repository(&scratch);
        let refused = LinkedWorktree::resolve(&repo).unwrap_err();
        assert_eq!(refused.status(), 1);
        assert!(refused.to_string().contains("main worktree"), "{refused}");
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_refused() {
        let scratch = Scratch::new();
        let refused = LinkedWorktree::resolve(&scratch.path().join("missing")).unwrap_err();
        assert_eq!(refused.status(), 1);
        assert!(
            refused.to_string().contains("is not a directory"),
            "{refused}"
        );
    }

    #[test]
    fn a_directory_outside_git_is_could_not_tell_naming_it() {
        let scratch = Scratch::new();
        let stop = LinkedWorktree::resolve(scratch.path()).unwrap_err();
        assert_eq!(stop.status(), 3);
        assert!(stop.to_string().contains("cannot inspect"), "{stop}");
    }
}
