//! Guard, then remove. The order matters: the remote branch is deleted last, and only
//! on request, because a chain that deletes it first and then fails locally leaves the
//! local branch and worktree alive with their upstream gone.

use std::env;
use std::path::{Path, PathBuf};

use crate::commands::guard::{self, OwnChanges, Rules};
use crate::git::{Git, shown, trimmed};
use crate::outcome::{CannotTell, Outcome, RefusalKind, Stop};
use crate::placement;
use crate::say::{Sink, say};
use crate::temp::TempDir;
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

pub fn run(path: &Path, rules: Rules, fate: BranchFate) -> Outcome {
    let own_changes = rules.own_changes;
    let default = fate != BranchFate::Keep && on_default_branch(&LinkedWorktree::resolve(path)?)?;
    let fate = if default { BranchFate::Keep } else { fate };
    // Before the guard, which empties submodules: "refused" must mean nothing happened.
    let integrated = if own_changes == OwnChanges::MustBeClean && fate != BranchFate::Keep {
        refuse_unmerged_branch(&LinkedWorktree::resolve(path)?)? == Merged::Integrated
    } else {
        false
    };
    guard::run(Some(path.to_path_buf()), rules)?;
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
    // No `git worktree prune`: `worktree remove` has already deleted this worktree's
    // registration, and prune works on the whole repository, where it would delete the
    // unpushed clones of any other stale registration. Sweep judges those one by one.
    say(Sink::Stdout, format_args!("removed {}", wt.display()));
    if let Ok(dir) = placement::dir_for(&worktree.common) {
        placement::remove_if_empty(&dir, Sink::Stdout);
    }

    if branch.is_empty() {
        say(
            Sink::Stdout,
            "the worktree was detached; no branch to delete",
        );
        return Ok(());
    }
    if default {
        say(
            Sink::Stdout,
            format_args!("kept branch {branch}: it is the default branch"),
        );
        return Ok(());
    }
    if fate == BranchFate::Keep {
        say(Sink::Stdout, format_args!("kept branch {branch}"));
        return Ok(());
    }
    if integrated {
        say(
            Sink::Stdout,
            format_args!(
                "branch {branch} has nothing its target lacks (squash-merged or rebased), so it is deleted though git calls it unmerged"
            ),
        );
    }
    delete_local_branch(main, &branch, own_changes, integrated)?;
    if fate == BranchFate::DeleteLocalAndRemote {
        delete_remote_branch(main, &remote, &branch)?;
    }
    Ok(())
}

/// Whether a worktree's branch is merged, by git's own rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merged {
    /// `git branch -d` agrees.
    Yes,
    /// `git branch -d` would refuse, but merging would add nothing: squashed or rebased.
    Integrated,
    No,
    NoBranch,
}

/// Asks git's own question, the one `git branch -d` will: merged into the branch's
/// upstream if it has one, or else into the HEAD of the checkout that runs the deletion.
pub fn merged_state(worktree: &LinkedWorktree, branch: &str) -> Result<Merged, CannotTell> {
    let main = &worktree.main_checkout;
    if branch.is_empty() {
        return Ok(Merged::NoBranch);
    }
    let target = merge_target(main, branch)?;
    match Git::at(
        main,
        ["merge-base", "--is-ancestor", branch, target.as_str()],
    )
    .exit_code(main)?
    {
        Some(0) => Ok(Merged::Yes),
        Some(1) => {
            // Into the main checkout as well: a branch with an upstream is squashed there.
            if adds_nothing(worktree, branch, &target)? || adds_nothing(worktree, branch, "HEAD")? {
                Ok(Merged::Integrated)
            } else {
                Ok(Merged::No)
            }
        }
        _ => Err(CannotTell::new(format!(
            "worktree: cannot inspect {}: git merge-base could not compare {branch} with {target}",
            main.display()
        ))),
    }
}

/// Whether merging `branch` into `target` would leave `target`'s tree as it is, which is
/// what a squash or a rebase leaves behind. Deleting such a branch loses no content.
///
/// By tree, not `git cherry`: patch ids ignore whitespace, so a branch that fixed only
/// indentation could match a commit that did not, and be deleted. A merge that conflicts
/// is "no", because the content cannot be shown to be there, and the branch is kept. So
/// is a squash that was reverted, because merging would bring the content back.
///
/// `merge-tree` writes the trees it makes, so they go to a scratch directory: `status`
/// asks this too, and a question changes nothing.
fn adds_nothing(worktree: &LinkedWorktree, branch: &str, target: &str) -> Result<bool, CannotTell> {
    let main = &worktree.main_checkout;
    let scratch = TempDir::create().map_err(|error| {
        CannotTell::new(format!(
            "worktree: cannot make a temporary directory: {error}"
        ))
    })?;
    let (code, merged) = Git::at(main, ["merge-tree", "--write-tree", target, branch])
        .objects_in(scratch.path(), &worktree.common.join("objects"))
        .code_and_output(main)?;
    match code {
        Some(0) => {
            let tree = format!("{target}^{{tree}}");
            let current = Git::at(main, ["rev-parse", "--verify", tree.as_str()]).probe(main)?;
            Ok(merged.trim_ascii_end() == current.trim_ascii_end())
        }
        Some(1) => Ok(false),
        _ => Err(CannotTell::new(format!(
            "worktree: cannot inspect {}: git merge-tree could not merge {branch} into {target}",
            main.display()
        ))),
    }
}

fn merge_target(main: &Path, branch: &str) -> Result<String, CannotTell> {
    let upstream = format!("{branch}@{{upstream}}");
    let has_upstream = Git::at(
        main,
        ["rev-parse", "--verify", "--quiet", upstream.as_str()],
    )
    .succeeds(main)?;
    Ok(if has_upstream {
        upstream
    } else {
        "HEAD".to_owned()
    })
}

/// Refuses now what `git branch -d` would refuse after the worktree is gone.
pub fn refuse_unmerged_branch(worktree: &LinkedWorktree) -> Outcome<Merged> {
    let wt = &worktree.path;
    let branch = shown(&Git::at(wt, ["branch", "--show-current"]).probe(wt)?);
    match merged_state(worktree, &branch)? {
        state @ (Merged::Yes | Merged::Integrated | Merged::NoBranch) => Ok(state),
        Merged::No => Err(Stop::refused_as(
            RefusalKind::UnmergedBranch,
            format!(
                "branch {branch} is not merged into {}, so nothing was removed — merge it, keep it with --keep-branch, or discard it with --force",
                merge_target(&worktree.main_checkout, &branch)?
            ),
        )),
    }
}

/// Whether the worktree has the repository's default branch checked out. Removing a
/// worktree never deletes that branch: it belongs to the repository, and once merged and
/// pushed `git branch -d` would delete it without a word.
pub fn on_default_branch(worktree: &LinkedWorktree) -> Result<bool, CannotTell> {
    let wt = &worktree.path;
    let branch = shown(&Git::at(wt, ["branch", "--show-current"]).probe(wt)?);
    is_default_branch(&worktree.main_checkout, &branch)
}

/// The default branch is the one the branch's remote names as its HEAD. With no remote
/// HEAD to read, the names git and the forges start a repository with stand in for it.
fn is_default_branch(main: &Path, branch: &str) -> Result<bool, CannotTell> {
    if branch.is_empty() {
        return Ok(false);
    }
    let remote = upstream_remote(main, branch)?;
    let head = format!("refs/remotes/{remote}/HEAD");
    match Git::at(main, ["symbolic-ref", "--quiet", head.as_str()]).code_and_output(main)? {
        (Some(0), target) => {
            let target = shown(&target);
            let prefix = format!("refs/remotes/{remote}/");
            return Ok(target.trim_end().strip_prefix(prefix.as_str()) == Some(branch));
        }
        // No remote HEAD: not fetched, or the remote is gone.
        (Some(1), _) => {}
        _ => {
            return Err(CannotTell::new(format!(
                "worktree: cannot inspect {}: git symbolic-ref could not read {head}",
                main.display()
            )));
        }
    }
    let configured = Git::at(main, ["config", "--get", "init.defaultBranch"]).answer(main)?;
    Ok(["main", "master"].contains(&branch)
        || configured.is_some_and(|name| shown(&name).trim_end() == branch))
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

fn delete_local_branch(
    main: &Path,
    branch: &str,
    own_changes: OwnChanges,
    integrated: bool,
) -> Outcome {
    let flag = if own_changes == OwnChanges::MayBeLost || integrated {
        "-D"
    } else {
        "-d"
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
