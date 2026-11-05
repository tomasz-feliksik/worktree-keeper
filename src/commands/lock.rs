//! `lock` and `unlock`: mark a worktree as occupied, with Git's own mechanism. Explicit
//! only: `create` does not lock, because a crashed agent would leave a lock nobody
//! releases, and the in-use check already protects a running one.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::git::Git;
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::say::{Sink, say};
use crate::worktree::LinkedWorktree;

pub fn lock(path: Option<PathBuf>, reason: Option<String>) -> Outcome {
    let worktree = resolve(path)?;
    let mut args: Vec<OsString> = vec!["worktree".into(), "lock".into()];
    if let Some(reason) = reason {
        args.extend(["--reason".into(), reason.into()]);
    }
    args.push(worktree.path.clone().into());
    act(&worktree, args, "locked")
}

pub fn unlock(path: Option<PathBuf>) -> Outcome {
    let worktree = resolve(path)?;
    let args: Vec<OsString> = vec![
        "worktree".into(),
        "unlock".into(),
        worktree.path.clone().into(),
    ];
    act(&worktree, args, "unlocked")
}

fn resolve(path: Option<PathBuf>) -> Outcome<LinkedWorktree> {
    let path = path.map_or_else(env::current_dir, Ok).map_err(|error| {
        CannotTell::new(format!(
            "worktree: cannot read the current directory: {error}"
        ))
    })?;
    LinkedWorktree::resolve(&path)
}

fn act(worktree: &LinkedWorktree, args: Vec<OsString>, done: &str) -> Outcome {
    let acted = Git::at(&worktree.main_checkout, args)
        .act(Sink::Stderr)
        .unwrap_or(false);
    if !acted {
        return Err(Stop::refused(format!(
            "{} was not {done}: git said why, above",
            worktree.path.display()
        )));
    }
    say(
        Sink::Stdout,
        format_args!("{done} {}", worktree.path.display()),
    );
    Ok(())
}
