//! The tracker actor for a directory: the name of the linked worktree it is in, or
//! nothing. One worktree hosts one agent at a time, so the worktree IS the actor,
//! for every harness at once. That is why it is derived here instead of written
//! into each harness's own settings file.
//!
//! Deliberately quiet, and the one command that may swallow a failed git: outside
//! git, or in a main checkout, there is no actor and the caller falls back to its
//! own default. Nothing here decides whether data is safe.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::git::{Git, trimmed, trimmed_path};
use crate::say::print_line;

pub fn run(dir: Option<PathBuf>) {
    let dir = dir.or_else(|| env::current_dir().ok());
    if let Some(name) = dir.as_deref().and_then(actor_of) {
        print_line(&name);
    }
}

fn actor_of(dir: &Path) -> Option<OsString> {
    if !dir.is_dir() {
        return None;
    }
    let ask = |args: &[&str]| Git::at(dir, args).answer(dir).ok().flatten();
    let gitdir = ask(&["rev-parse", "--absolute-git-dir"])?;
    let common = ask(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    if trimmed(gitdir) == trimmed(common) {
        return None;
    }
    let top = ask(&["rev-parse", "--show-toplevel"])?;
    trimmed_path(top).file_name().map(ToOwned::to_owned)
}
