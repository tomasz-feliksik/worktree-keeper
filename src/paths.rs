//! Paths read from Git's pointers, and filesystem questions asked strictly.
//!
//! Under `worktree.useRelativePaths`, each pointer is relative to the directory
//! **holding it**, never the current directory: `<worktree>/.git` from the
//! worktree, and `<admin dir>/gitdir` from the admin directory. Getting that wrong
//! once made `sweep --apply` unregister live worktrees. So a [`ResolvedPath`] can
//! only be built by naming the holder, and every pointer is read through the
//! functions here.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::git::trimmed;
use crate::outcome::CannotTell;

/// More symlinks than this on one path is a cycle, as for the kernel's `ELOOP`.
const MAX_SYMLINKS: u32 = 40;

/// An absolute path with every symlink, `.` and `..` resolved, so that two of them
/// can be compared. The port of `resolve_from`, which used `realpath -m`: the
/// path need not exist, and what does not exist is taken lexically.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct ResolvedPath(PathBuf);

impl ResolvedPath {
    /// Resolves `pointer` as a pointer held in the directory `holder`.
    pub fn resolve(holder: &Path, pointer: &OsStr) -> Result<Self, CannotTell> {
        let joined = std::path::absolute(holder)
            .map(|holder| holder.join(pointer))
            .map_err(|error| cannot_read(holder, &error))?;
        canonical_allowing_missing(&joined)
            .map(Self)
            .map_err(|error| cannot_read(&joined, &error))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Where `<dir>/.git` points, if it is a file holding a `gitdir:` line.
pub fn read_gitfile(dir: &Path) -> Result<Option<ResolvedPath>, CannotTell> {
    let file = dir.join(".git");
    if !is_file(&file)? {
        return Ok(None);
    }
    let contents = fs::read(&file).map_err(|error| cannot_read(&file, &error))?;
    let pointer = contents
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"gitdir: "))
        .filter(|pointer| !pointer.is_empty());
    pointer
        .map(|pointer| ResolvedPath::resolve(dir, OsStr::from_bytes(pointer)))
        .transpose()
}

/// Where a registration's `<admin dir>/gitdir` points: its worktree's `.git`.
#[allow(dead_code)] // `sweep` reads it.
pub fn read_backpointer(admin_dir: &Path) -> Result<ResolvedPath, CannotTell> {
    let file = admin_dir.join("gitdir");
    let contents = fs::read(&file).map_err(|error| cannot_read(&file, &error))?;
    ResolvedPath::resolve(admin_dir, &trimmed(contents))
}

/// `[[ -e path ]]`, except that being unable to look is not "no".
pub fn exists(path: &Path) -> Result<bool, CannotTell> {
    Ok(metadata(path)?.is_some())
}

/// `[[ -d path ]]`, except that being unable to look is not "no".
pub fn is_dir(path: &Path) -> Result<bool, CannotTell> {
    Ok(metadata(path)?.is_some_and(|found| found.is_dir()))
}

/// `[[ -f path ]]`, except that being unable to look is not "no".
pub fn is_file(path: &Path) -> Result<bool, CannotTell> {
    Ok(metadata(path)?.is_some_and(|found| found.is_file()))
}

/// The real directories directly inside `dir`, not symlinks to them, sorted.
#[allow(dead_code)] // `sweep` reads it.
pub fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>, CannotTell> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(|error| cannot_read(dir, &error))? {
        let entry = entry.map_err(|error| cannot_read(dir, &error))?;
        let kind = entry
            .file_type()
            .map_err(|error| cannot_read(&entry.path(), &error))?;
        if kind.is_dir() {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// The report for a filesystem question that could not be answered.
pub fn cannot_read(path: &Path, error: &io::Error) -> CannotTell {
    CannotTell::new(format!(
        "worktree: cannot inspect {} — refusing, because an unanswerable probe is not the same as a safe one: {error}",
        path.display()
    ))
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, CannotTell> {
    match fs::metadata(path) {
        Ok(found) => Ok(Some(found)),
        Err(error) if is_absent(&error) => Ok(None),
        Err(error) => Err(cannot_read(path, &error)),
    }
}

fn is_absent(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

fn canonical_allowing_missing(path: &Path) -> io::Result<PathBuf> {
    let mut resolved = PathBuf::from("/");
    let mut pending: Vec<PathBuf> = vec![path.to_path_buf()];
    let mut symlinks = 0;
    while let Some(next) = pending.pop() {
        let mut components = next.components();
        let Some(component) = components.next() else {
            continue;
        };
        pending.push(components.as_path().to_path_buf());
        match component {
            Component::RootDir | Component::Prefix(_) => resolved = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                match fs::read_link(&candidate) {
                    Ok(target) => {
                        symlinks += 1;
                        if symlinks > MAX_SYMLINKS {
                            return Err(io::Error::other(format!(
                                "too many levels of symbolic links at {}",
                                candidate.display()
                            )));
                        }
                        pending.push(target);
                    }
                    // EINVAL is how readlink says "exists, and not a symlink".
                    Err(error) if error.kind() == ErrorKind::InvalidInput => resolved = candidate,
                    Err(error) if is_absent(&error) => resolved = candidate,
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;
    use std::os::unix::fs::symlink;

    fn resolve(holder: &Path, pointer: &str) -> PathBuf {
        ResolvedPath::resolve(holder, OsStr::new(pointer))
            .unwrap()
            .as_path()
            .to_path_buf()
    }

    #[test]
    fn a_relative_pointer_resolves_from_the_directory_holding_it() {
        let scratch = Scratch::new();
        let holder = scratch.path().join("a/b");
        fs::create_dir_all(&holder).unwrap();
        assert_eq!(resolve(&holder, "../c"), scratch.path().join("a/c"));
    }

    #[test]
    fn a_relative_pointer_ignores_the_current_directory() {
        // The current directory of a test run is the crate root, never here.
        assert_eq!(
            resolve(Path::new("/holder/dir"), "x"),
            Path::new("/holder/dir/x")
        );
    }

    #[test]
    fn an_absolute_pointer_ignores_the_holder() {
        assert_eq!(
            resolve(Path::new("/holder"), "/other/place"),
            Path::new("/other/place")
        );
    }

    #[test]
    fn a_path_that_does_not_exist_is_taken_lexically() {
        assert_eq!(
            resolve(Path::new("/nowhere"), "a/./b/../c"),
            Path::new("/nowhere/a/c")
        );
    }

    #[test]
    fn a_symlink_is_followed_before_its_parent_is_taken() {
        let scratch = Scratch::new();
        let real = scratch.path().join("deep/real");
        fs::create_dir_all(&real).unwrap();
        symlink(&real, scratch.path().join("link")).unwrap();
        assert_eq!(
            resolve(scratch.path(), "link/../x"),
            scratch.path().join("deep/x")
        );
    }

    #[test]
    fn two_spellings_of_one_place_compare_equal() {
        let scratch = Scratch::new();
        let real = scratch.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, scratch.path().join("link")).unwrap();
        assert_eq!(
            resolve(scratch.path(), "link/f"),
            resolve(&real, "../real/f")
        );
    }

    #[test]
    fn a_symlink_cycle_is_could_not_tell_rather_than_a_hang() {
        let scratch = Scratch::new();
        symlink("b", scratch.path().join("a")).unwrap();
        symlink("a", scratch.path().join("b")).unwrap();
        assert!(ResolvedPath::resolve(scratch.path(), OsStr::new("a")).is_err());
    }

    #[test]
    fn a_gitfile_resolves_relative_to_its_own_directory() {
        let scratch = Scratch::new();
        let dir = scratch.path().join("wt");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(".git"), "gitdir: ../admin/one\n").unwrap();
        let pointer = read_gitfile(&dir).unwrap().unwrap();
        assert_eq!(pointer.as_path(), scratch.path().join("admin/one"));
    }

    #[test]
    fn a_gitfile_may_hold_an_absolute_pointer() {
        let scratch = Scratch::new();
        fs::write(scratch.path().join(".git"), "gitdir: /elsewhere/x\n").unwrap();
        let pointer = read_gitfile(scratch.path()).unwrap().unwrap();
        assert_eq!(pointer.as_path(), Path::new("/elsewhere/x"));
    }

    #[test]
    fn no_gitfile_and_a_gitfile_with_no_pointer_are_both_none() {
        let scratch = Scratch::new();
        assert_eq!(read_gitfile(scratch.path()).unwrap(), None);
        fs::write(scratch.path().join(".git"), "not a pointer\n").unwrap();
        assert_eq!(read_gitfile(scratch.path()).unwrap(), None);
    }

    #[test]
    fn a_git_directory_is_not_a_gitfile() {
        let scratch = Scratch::new();
        fs::create_dir(scratch.path().join(".git")).unwrap();
        assert_eq!(read_gitfile(scratch.path()).unwrap(), None);
    }

    #[test]
    fn a_backpointer_resolves_relative_to_the_admin_directory() {
        let scratch = Scratch::new();
        let admin = scratch.path().join("repo/.git/worktrees/one");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("gitdir"), "../../../../wt/.git\n").unwrap();
        let pointer = read_backpointer(&admin).unwrap();
        assert_eq!(pointer.as_path(), scratch.path().join("wt/.git"));
    }

    #[test]
    fn asking_through_a_file_is_a_no_and_a_missing_path_is_a_no() {
        let scratch = Scratch::new();
        let file = scratch.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(!exists(&file.join("under")).unwrap());
        assert!(!exists(&scratch.path().join("missing")).unwrap());
        assert!(exists(&file).unwrap());
    }

    #[test]
    fn a_symlink_cycle_cannot_be_asked_about() {
        let scratch = Scratch::new();
        symlink("loop", scratch.path().join("loop")).unwrap();
        assert!(exists(&scratch.path().join("loop")).is_err());
    }

    #[test]
    fn file_and_directory_questions_tell_the_two_apart() {
        let scratch = Scratch::new();
        let file = scratch.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(is_file(&file).unwrap() && !is_dir(&file).unwrap());
        assert!(is_dir(scratch.path()).unwrap() && !is_file(scratch.path()).unwrap());
    }

    #[test]
    fn subdirectories_lists_real_directories_only_and_in_order() {
        let scratch = Scratch::new();
        for name in ["b", "a"] {
            fs::create_dir(scratch.path().join(name)).unwrap();
        }
        fs::write(scratch.path().join("file"), "x").unwrap();
        symlink(scratch.path().join("a"), scratch.path().join("link")).unwrap();
        assert_eq!(
            subdirectories(scratch.path()).unwrap(),
            [scratch.path().join("a"), scratch.path().join("b")]
        );
    }

    #[test]
    fn listing_a_missing_directory_is_could_not_tell() {
        assert!(subdirectories(Path::new("/nonexistent-directory")).is_err());
    }
}
