//! Adds `<root>/<repo>/<slug>`, then provisions it. Its callers read stdout as the
//! path and nothing else, so git's output and every message go to stderr.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::provision;
use crate::git::{Git, shown, trimmed_path};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::paths::{self, cannot_read};
use crate::say::{Sink, print_path, say};
use crate::worktree::parent_of;

pub fn run(branch: &str, base: Option<&str>) -> Outcome {
    if branch.is_empty() {
        return Err(Stop::Usage(
            "worktree: create: the branch name is empty".to_owned(),
        ));
    }
    let root = placement_root()?;
    let repo = repository_root()?;
    let name = repo.file_name().ok_or_else(|| {
        CannotTell::new(format!(
            "worktree: {} has no directory name",
            repo.display()
        ))
    })?;
    let path = root.join(name).join(slug_of(branch));

    if paths::exists(&path)? {
        check_existing(&path, branch)?;
        say(
            Sink::Stderr,
            format_args!("{} exists — not creating it again", path.display()),
        );
    } else {
        add(&repo, &path, branch, base.unwrap_or("HEAD"))?;
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

fn placement_root() -> Outcome<PathBuf> {
    env::var_os("WORKTREE_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            Stop::refused("no placement root: set WORKTREE_ROOT (worktreeKeeper.root sets it for the installed command)")
        })
}

/// The directory holding the shared git directory of the repository we are in.
fn repository_root() -> Outcome<PathBuf> {
    let common = Git::new(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .answer(Path::new("."))?
        .ok_or_else(|| Stop::refused("not inside a git repository"))?;
    Ok(parent_of(&trimmed_path(common))?)
}

fn add(repo: &Path, path: &Path, branch: &str, base: &str) -> Outcome {
    let parent = parent_of(path)?;
    fs::create_dir_all(&parent)
        .map_err(|error| Stop::refused(format!("cannot create {}: {error}", parent.display())))?;

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
