//! Submodules, then gated direnv trust, in the order they must run: a populated
//! tree before anything reads it, and trust last so a broken tree is never trusted.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::git::{Git, trimmed_path};
use crate::outcome::{Outcome, Stop};
use crate::paths;
use crate::say::{Sink, say};
use crate::worktree::parent_of;

pub fn run(path: &Path, sink: Sink) -> Outcome {
    if !paths::is_dir(path)? {
        return Err(Stop::refused(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    submodules(path, sink)?;
    direnv(path, sink)
}

/// `git worktree add` leaves submodules uninitialised (observed 2026-08-24: the
/// first `nix develop` in a fresh worktree failed on an empty submodule), and
/// `submodule.recurse=true` does not cover worktree creation.
///
/// Never fails the caller: workmux's `post_create` is fail-fast (a non-zero hook
/// aborts every remaining one and leaves the worktree half-created, measured on
/// 0.1.240), so an unreachable submodule remote must not cost the worktree its
/// direnv trust below.
fn submodules(path: &Path, sink: Sink) -> Outcome {
    if !paths::is_file(&path.join(".gitmodules"))? {
        return Ok(());
    }
    let initialised = Git::new(["submodule", "update", "--init", "--recursive"])
        .current_dir(path)
        .act(sink)
        .unwrap_or(false);
    if initialised {
        say(sink, "submodules initialised");
    } else {
        say(
            sink,
            "submodule init FAILED (offline?) — run 'git submodule update --init --recursive' by hand before 'nix develop'",
        );
    }
    Ok(())
}

/// Inherits direnv trust into a fresh worktree, only when provably safe.
///
/// direnv's allow database is keyed by path and content (verified 2026-08-24
/// against 2.37.1), so a worktree is never trusted merely by existing. This allows
/// the worktree's `.envrc` if and only if the main checkout's `.envrc` is itself
/// currently allowed and the worktree's copy is byte-identical to it: exactly the
/// bytes the user already reviewed. A worktree made from another base can carry a
/// different `.envrc`; that one gets a message and a manual review.
///
/// Deliberately not a `whitelist.prefix` on the placement root, which would trust
/// every future `.envrc` under the path, content unseen.
fn direnv(path: &Path, sink: Sink) -> Outcome {
    if !paths::is_file(&path.join(".envrc"))? {
        return Ok(());
    }
    let Some(root) = main_checkout(path)? else {
        return Ok(());
    };
    if !paths::is_file(&root.join(".envrc"))? || !allowed(&root) {
        return Ok(());
    }
    if !same_bytes(&path.join(".envrc"), &root.join(".envrc")) {
        say(
            sink,
            "direnv: this worktree's .envrc differs from the allowed root copy — review it and run 'direnv allow' yourself",
        );
        return Ok(());
    }
    let allow = Command::new("direnv")
        .args(["allow", "."])
        .current_dir(path)
        .stdout(sink.stdio())
        .status();
    if !allow.is_ok_and(|status| status.success()) {
        return Err(Stop::refused(format!(
            "direnv allow failed in {}",
            path.display()
        )));
    }
    say(
        sink,
        "direnv: worktree .envrc allowed (byte-identical to the allowed root .envrc)",
    );
    Ok(())
}

fn main_checkout(path: &Path) -> Outcome<Option<PathBuf>> {
    let common = Git::new(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(path)
        .answer(path)?;
    match common {
        Some(common) => Ok(Some(parent_of(&trimmed_path(common))?)),
        None => Ok(None),
    }
}

/// Gate 1: the main checkout's `.envrc` is allowed. direnv 2.37 prints the numeric
/// enum, 0 for allowed; older versions print `true`. direnv missing or failing is
/// not allowed.
fn allowed(root: &Path) -> bool {
    Command::new("direnv")
        .arg("status")
        .current_dir(root)
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| status_says_allowed(&output.stdout))
}

fn status_says_allowed(status: &[u8]) -> bool {
    status
        .split(|byte| *byte == b'\n')
        .any(|line| line == b"Found RC allowed 0" || line == b"Found RC allowed true")
}

/// Gate 2: the same bytes the user already reviewed. Unreadable is not the same.
fn same_bytes(left: &Path, right: &Path) -> bool {
    match (fs::read(left), fs::read(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_spellings_of_allowed_are_accepted_and_nothing_else() {
        assert!(status_says_allowed(b"x\nFound RC allowed 0\ny\n"));
        assert!(status_says_allowed(b"Found RC allowed true\n"));
        assert!(!status_says_allowed(b"Found RC allowed 1\n"));
        assert!(!status_says_allowed(b"Found RC allowed false\n"));
        assert!(!status_says_allowed(b"Found RC allowed 01\n"));
        assert!(!status_says_allowed(b""));
    }
}
