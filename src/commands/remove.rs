//! Guard, then remove. The order matters: the remote branch is deleted last, and only
//! on request, because a chain that deletes it first and then fails locally leaves the
//! local branch and worktree alive with their upstream gone.

use std::env;
use std::path::{Path, PathBuf};

use crate::commands::guard::{self, OwnChanges};
use crate::git::{Git, shown, trimmed};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::say::{Sink, say};
use crate::worktree::LinkedWorktree;

/// `ls-remote --exit-code`'s status for "no matching refs".
const NO_MATCHING_REFS: i32 = 2;

/// What to do with the worktree's branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchFate {
    Keep,
    DeleteLocal,
    DeleteLocalAndRemote,
}

pub fn run(path: &Path, own_changes: OwnChanges, fate: BranchFate) -> Outcome {
    guard::run(Some(path.to_path_buf()), own_changes)?;
    let worktree = LinkedWorktree::resolve(path)?;
    let wt = &worktree.path;
    let main = &worktree.main_checkout;
    let branch = shown(&Git::at(wt, ["branch", "--show-current"]).probe(wt)?);

    if env::current_dir().is_ok_and(|cwd| cwd.starts_with(wt)) {
        say(
            Sink::Stderr,
            format_args!(
                "note: your shell's cwd is inside {} — cd elsewhere afterwards",
                wt.display()
            ),
        );
    }
    // Detached first, so that `git branch -d` can delete the branch afterwards.
    if !branch.is_empty() {
        Git::at(wt, ["checkout", "-q", "--detach"]).probe(wt)?;
    }

    // Read before the branch goes: deleting it deletes its configuration too.
    let remote = upstream_remote(main, &branch)?;

    let mut args: Vec<PathBuf> = vec!["worktree".into(), "remove".into()];
    if own_changes == OwnChanges::MayBeLost {
        args.push("--force".into());
    }
    args.push(wt.clone());
    Git::at(main, args).probe(wt)?;
    Git::at(main, ["worktree", "prune"]).probe(main)?;
    say(Sink::Stdout, format_args!("removed {}", wt.display()));

    if branch.is_empty() {
        say(
            Sink::Stdout,
            "the worktree was detached; no branch to delete",
        );
        return Ok(());
    }
    if fate == BranchFate::Keep {
        say(Sink::Stdout, format_args!("kept branch {branch}"));
        return Ok(());
    }
    delete_local_branch(main, &branch, own_changes)?;
    if fate == BranchFate::DeleteLocalAndRemote {
        delete_remote_branch(main, &remote, &branch)?;
    }
    Ok(())
}

fn upstream_remote(main: &Path, branch: &str) -> Result<String, CannotTell> {
    if branch.is_empty() {
        return Ok(String::new());
    }
    let key = format!("branch.{branch}.remote");
    let configured = Git::at(main, ["config", "--get", key.as_str()]).answer(main)?;
    Ok(configured.map_or_else(
        || "origin".to_owned(),
        |remote| trimmed(remote).to_string_lossy().into_owned(),
    ))
}

fn delete_local_branch(main: &Path, branch: &str, own_changes: OwnChanges) -> Outcome {
    let flag = match own_changes {
        OwnChanges::MayBeLost => "-D",
        OwnChanges::MustBeClean => "-d",
    };
    let deleted = Git::at(main, ["branch", flag, branch])
        .act(Sink::Stdout)
        .unwrap_or(false);
    if deleted {
        return Ok(());
    }
    Err(match own_changes {
        OwnChanges::MustBeClean => Stop::refused(format!(
            "branch {branch} is not merged — the worktree is gone; delete the branch with 'git branch -D {branch}' once you have decided"
        )),
        OwnChanges::MayBeLost => Stop::refused(format!("could not delete branch {branch}")),
    })
}

fn delete_remote_branch(main: &Path, remote: &str, branch: &str) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let found = Git::at(
        main,
        [
            "ls-remote",
            "--exit-code",
            "--heads",
            remote,
            reference.as_str(),
        ],
    )
    .exit_code(main)?;
    match found {
        Some(0) => {
            let pushed = Git::at(main, ["push", remote, "--delete", branch])
                .act(Sink::Stdout)
                .unwrap_or(false);
            if pushed {
                Ok(())
            } else {
                Err(Stop::refused(format!("could not delete {branch} on {remote}")))
            }
        }
        Some(NO_MATCHING_REFS) => {
            say(
                Sink::Stdout,
                format_args!("{remote} has no branch {branch}; nothing to delete there"),
            );
            Ok(())
        }
        // The remote did not answer, which is not the same as "it has no such branch".
        _ => Err(CannotTell::new(format!(
            "worktree: cannot inspect {remote} for branch {branch}, so it was left alone: git ls-remote failed"
        ))
        .into()),
    }
}
