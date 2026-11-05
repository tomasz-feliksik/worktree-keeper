//! A throwaway file, removed when dropped. `std` has no temporary files, and the
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

#[derive(Debug)]
pub struct TempFile(PathBuf);

impl TempFile {
    /// Creates a new empty file. `create_new` refuses to follow a planted symlink.
    pub fn create() -> io::Result<Self> {
        Self::create_in(&env::temp_dir())
    }

    fn create_in(dir: &Path) -> io::Result<Self> {
        let pid = process::id();
        for _ in 0..ATTEMPTS {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("worktree-keeper-{pid}-{serial}"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self(path)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("no free temporary file name"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

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
