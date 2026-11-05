//! Adds `<parent>/<repo>.worktrees/<slug>`, then provisions it. Its callers read stdout as the
//! path and nothing else, so git's output and every message go to stderr.

use std::ffi::OsString;
use std::path::Path;

use crate::commands::provision;
use crate::git::{Git, shown, trimmed_path};
use crate::outcome::{Outcome, Stop};
use crate::paths::{self, cannot_read};
use crate::placement;
use crate::say::{Sink, print_path, say};
use crate::worktree::parent_of;

pub fn run(branch: &str, base: Option<&str>) -> Outcome {
    if branch.is_empty() {
        return Err(Stop::Usage(
            "worktree: create: the branch name is empty".to_owned(),
        ));
    }
    let common = placement::common_here()?;
    let dir = placement::dir_for(&common)?;
    let repo = parent_of(&common)?;
    let path = dir.join(slug_of(branch));

    if paths::exists(&path)? {
        check_existing(&path, branch)?;
        say(
            Sink::Stderr,
            format_args!("{} exists — not creating it again", path.display()),
        );
    } else {
        add(&repo, &path, branch, base.unwrap_or("HEAD"))?;
        note_unless_relative(&repo, &dir);
        provision::run(&path, Sink::Stderr)?;
    }
    print_path(&path)
        .map_err(|error| Stop::refused(format!("cannot print {}: {error}", path.display())))
}

/// `feat/x` and `feat-x` share a slug, so an existing path may be another branch's.
/// Handing it back would let an agent work on the wrong branch, believing otherwise.
fn check_existing(path: &Path, branch: &str) -> Outcome {
    let top = Git::at(path, ["rev-parse", "--show-toplevel"]).answer(path)?;
    let here = path
        .canonicalize()
        .map_err(|error| cannot_read(path, &error))?;
    if top.map(trimmed_path) != Some(here) {
        return Err(Stop::refused(format!(
            "{} exists and is not a worktree, so it cannot be used for branch {branch}",
            path.display()
        )));
    }
    let current = shown(&Git::at(path, ["branch", "--show-current"]).probe(path)?);
    if current == branch {
        return Ok(());
    }
    let current = if current.is_empty() {
        "a detached HEAD"
    } else {
        current.as_str()
    };
    Err(Stop::refused(format!(
        "{} exists on {current}, not on the requested branch {branch}: their names share a slug",
        path.display()
    )))
}

/// Absolute links break when the repository and its worktrees move together. Only ever
/// a note: the worktree exists by now, and the caller is waiting for its path.
fn note_unless_relative(repo: &Path, dir: &Path) {
    let setting = Git::at(
        repo,
        [
            "config",
            "--type=bool",
            "--get",
            "worktree.useRelativePaths",
        ],
    )
    .answer(repo);
    if !matches!(setting, Ok(Some(value)) if value.trim_ascii() == b"true") {
        say(
            Sink::Stderr,
            format_args!(
                "note: worktree.useRelativePaths is not true, so moving {} and {} together breaks this worktree's links until 'git worktree repair'. 'git config --global worktree.useRelativePaths true' avoids that for new worktrees",
                repo.display(),
                dir.display()
            ),
        );
    }
}

fn add(repo: &Path, path: &Path, branch: &str, base: &str) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let exists = Git::at(
        repo,
        ["show-ref", "--verify", "--quiet", reference.as_str()],
    )
    .succeeds(repo)?;
    let mut args: Vec<OsString> = vec!["worktree".into(), "add".into()];
    if exists {
        args.extend([path.into(), branch.into()]);
    } else {
        args.extend(["-b".into(), branch.into(), path.into(), base.into()]);
    }
    if Git::at(repo, args).act(Sink::Stderr).unwrap_or(false) {
        Ok(())
    } else {
        Err(Stop::refused(format!(
            "git worktree add failed for {} in {}",
            path.display(),
            repo.display()
        )))
    }
}

fn slug_of(branch: &str) -> String {
    branch.replace(['/', ' '], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slashes_and_spaces_become_dashes_and_nothing_else_changes() {
        assert_eq!(slug_of("feat/some thing"), "feat-some-thing");
        assert_eq!(slug_of("a.b_c-d"), "a.b_c-d");
    }
}
