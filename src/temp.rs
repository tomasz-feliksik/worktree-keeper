//! A throwaway file or directory, removed when dropped. `std` has no temporary files, and the
//! runtime crates stay at `clap`, `serde` and `serde_json`.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};

const ATTEMPTS: u32 = 1000;

/// A name is never reused within a process, even after its file has gone.
static NEXT: AtomicU32 = AtomicU32::new(0);

/// Tries names until `make` finds one free. Only a name that is taken moves on: `make`
/// has to refuse an existing path, which is what stops a planted symlink being followed.
fn new_name_in(dir: &Path, make: impl Fn(&Path) -> io::Result<()>) -> io::Result<PathBuf> {
    let pid = process::id();
    for _ in 0..ATTEMPTS {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("worktree-keeper-{pid}-{serial}"));
        match make(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no free temporary name"))
}

#[derive(Debug)]
pub struct TempFile(PathBuf);

impl TempFile {
    /// Creates a new empty file. `create_new` refuses to follow a planted symlink.
    pub fn create() -> io::Result<Self> {
        Self::create_in(&env::temp_dir())
    }

    fn create_in(dir: &Path) -> io::Result<Self> {
        new_name_in(dir, |path| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(drop)
        })
        .map(Self)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        // Best effort: a leftover empty file in the temp directory harms nothing.
        drop(fs::remove_file(&self.0));
    }
}

/// A new empty directory, removed with what is in it when dropped.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn create() -> io::Result<Self> {
        Self::create_in(&env::temp_dir())
    }

    fn create_in(dir: &Path) -> io::Result<Self> {
        new_name_in(dir, |path| fs::create_dir(path)).map(Self)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort, as for a file: what is left harms nothing.
        drop(fs::remove_dir_all(&self.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    #[test]
    fn a_temp_directory_is_removed_with_its_contents() {
        let dir = TempDir::create().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("inside"), "x").unwrap();
        assert!(path.is_dir());
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn a_name_left_by_an_earlier_process_is_skipped() {
        let scratch = Scratch::new();
        let next = NEXT.load(Ordering::Relaxed);
        for serial in next..next + 4 {
            let stale = format!("worktree-keeper-{}-{serial}", process::id());
            fs::write(scratch.path().join(stale), "stale").unwrap();
        }
        let file = TempFile::create_in(scratch.path()).unwrap();
        assert_eq!(fs::read(file.path()).unwrap(), b"");
    }

    #[test]
    fn a_temp_directory_reports_a_failure_as_itself() {
        let scratch = Scratch::new();
        let error = TempDir::create_in(&scratch.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn any_other_failure_is_reported_as_itself() {
        let scratch = Scratch::new();
        let error = TempFile::create_in(&scratch.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn a_temp_file_exists_while_held_and_is_gone_after() {
        let file = TempFile::create().unwrap();
        let path = file.path().to_path_buf();
        assert!(path.is_file());
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn two_temp_files_never_share_a_path() {
        let (one, two) = (TempFile::create().unwrap(), TempFile::create().unwrap());
        assert_ne!(one.path(), two.path());
    }
}
