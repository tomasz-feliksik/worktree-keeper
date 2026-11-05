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
        let dir = env::temp_dir();
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
