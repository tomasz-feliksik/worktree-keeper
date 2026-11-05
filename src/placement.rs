//! Where a repository's worktrees live: `<parent>/<repo>.worktrees/<slug>`, beside the
//! main checkout. One rule and no setting, so `create`, `status`, `sweep` and `remove`
//! cannot disagree. Inside the checkout would be simpler to find, but `git clean -ffdx`
//! there deletes uncommitted work without any guard running.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::git::{Git, trimmed_path};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::say::{Sink, say};
use crate::worktree::parent_of;

const SUFFIX: &str = ".worktrees";

/// The shared git directory of the repository the caller is in.
pub fn common_here() -> Outcome<PathBuf> {
    let common = Git::new(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .answer(Path::new("."))?
        .ok_or_else(|| Stop::refused("not inside a git repository"))?;
    Ok(trimmed_path(common))
}

/// The directory holding a repository's worktrees. Only a checkout has one: the parent of
/// a submodule's git directory is inside `.git`, and a bare repository's is unrelated.
pub fn dir_for(common: &Path) -> Outcome<PathBuf> {
    if common.file_name() != Some(OsStr::new(".git")) {
        return Err(Stop::refused(format!(
            "{} is not the .git directory of a checkout (a submodule or bare repository?), so its worktrees have no place beside it",
            common.display()
        )));
    }
    let main = parent_of(common)?;
    let name = main.file_name().ok_or_else(|| {
        CannotTell::new(format!(
            "worktree: {} has no directory name",
            main.display()
        ))
    })?;
    let mut dir = name.to_owned();
    dir.push(SUFFIX);
    Ok(parent_of(&main)?.join(dir))
}

/// Whether a directory is named as [`dir_for`] names one.
pub fn is_worktrees_dir(dir: &Path) -> bool {
    dir.file_name()
        .is_some_and(|name| name.as_bytes().ends_with(SUFFIX.as_bytes()))
}

/// Tidies the directory once nothing is left in it. `rmdir` is the whole test, so
/// anything inside, even something the user put there, keeps it. A failure only warns:
/// the worktree it follows is already gone.
pub fn remove_if_empty(dir: &Path, sink: Sink) {
    match fs::remove_dir(dir) {
        Ok(()) => say(sink, format_args!("removed empty {}", dir.display())),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => say(
            sink,
            format_args!("could not remove {}: {error}", dir.display()),
        ),
    }
}

/// Every linked worktree git has registered, wherever it lives. The main checkout is
/// always the first entry, and is left out.
pub fn linked_worktrees(main: &Path) -> Result<Vec<PathBuf>, CannotTell> {
    let listing = Git::at(main, ["worktree", "list", "--porcelain", "-z"]).probe(main)?;
    Ok(listing
        .split(|byte| *byte == 0)
        .filter_map(|attribute| attribute.strip_prefix(b"worktree "))
        .skip(1)
        .map(|path| PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
        .collect())
}
