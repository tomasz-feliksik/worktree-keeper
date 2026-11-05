//! May this worktree be removed? Removal destroys two things nothing else keeps:
//! the worktree's uncommitted changes, and its submodule clones. Each worktree gets
//! its own under `<gitdir>/modules`, and a commit made in one exists nowhere else
//! until pushed. The worktree's own commits are not at risk here: they live in the
//! shared repository, and whether its branch is deleted is the removing tool's
//! decision.
//!
//! Then clears `<gitdir>/modules`: git refuses `worktree remove` on that directory's
//! mere existence, and nothing else ever deletes it. That refusal is reached only
//! without `--force`, which goes straight past it. So the value of this guard is the
//! unpushed check, not getting past git. Never "simplify" it into `--force`: that
//! deletes submodule clones unchecked.
//!
//! `--force` accepts losing the worktree's own uncommitted changes. Submodule changes
//! and unpushed submodule commits are refused regardless.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::git::{Git, flag, shown, trimmed};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::paths::{self, cannot_read};
use crate::say::{Sink, say};
use crate::submodules::{self, nested_in};
use crate::temp::TempFile;
use crate::worktree::LinkedWorktree;

/// Whether the worktree's own uncommitted changes may be lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnChanges {
    MustBeClean,
    MayBeLost,
}

/// Proof that every file and clone was found disposable. Only [`assess`] can make
/// one, and [`clear`] needs one, so nothing can be emptied before it was checked.
#[derive(Debug)]
#[must_use]
pub struct Clearance {
    top_level: Vec<PathBuf>,
    stranded: Vec<PathBuf>,
}

/// `guard`: assess, then clear. Never removes the worktree.
pub fn run(path: Option<PathBuf>, own_changes: OwnChanges) -> Outcome {
    let path = path
        .or_else(|| {
            env::var_os("WM_WORKTREE_PATH")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        })
        .map_or_else(env::current_dir, Ok)
        .map_err(|error| {
            CannotTell::new(format!(
                "worktree: cannot read the current directory: {error}"
            ))
        })?;
    let worktree = LinkedWorktree::resolve(&path)?;
    let clearance = assess(&worktree, own_changes)?;
    clear(&worktree, clearance)?;
    say(
        Sink::Stdout,
        format_args!("{} may be removed", worktree.path.display()),
    );
    Ok(())
}

/// Every check, changing nothing on disk.
pub fn assess(worktree: &LinkedWorktree, own_changes: OwnChanges) -> Outcome<Clearance> {
    let wt = &worktree.path;
    if own_changes == OwnChanges::MustBeClean {
        let dirty = status(wt)?;
        if !dirty.is_empty() {
            return Err(Stop::refused(format!(
                "{} has uncommitted changes — commit them, or discard them with 'worktree remove --force {}':\n{}",
                wt.display(),
                wt.display(),
                shown(&dirty)
            )));
        }
    }

    let subs = submodules::populated(wt)?;
    let mut live = Vec::new();
    let mut stranded = Vec::new();
    for sub in &subs {
        if submodules::is_stranded(&wt.join(sub))? {
            stranded.push(sub.clone());
        } else {
            live.push(sub.clone());
        }
    }
    for sub in &live {
        let dirty = status(&wt.join(sub))?;
        if !dirty.is_empty() {
            return Err(Stop::refused(format!(
                "submodule {} in {} has uncommitted changes — commit and push them there first:\n{}",
                sub.display(),
                wt.display(),
                shown(&dirty)
            )));
        }
    }

    if !stranded.is_empty() {
        let head = shown(&Git::at(wt, ["rev-parse", "HEAD"]).probe(wt)?);
        for sub in &stranded {
            verify_top_level_stranded(worktree, sub, &subs, &head)?;
        }
    }

    for clone in submodules::module_gitdirs(&worktree.gitdir)? {
        let pending = submodules::unpushed(&clone)?;
        if !pending.is_empty() {
            return Err(Stop::refused(format!(
                "{} holds commits no remote has — a submodule clone that exists only in this worktree; push them first:\n{}",
                clone.display(),
                shown(&pending)
            )));
        }
    }

    let top_level = live
        .into_iter()
        .filter(|sub| !nested_in(sub, &subs))
        .collect();
    Ok(Clearance {
        top_level,
        stranded,
    })
}

/// With every clone proven disposable: empties the submodules and clears `modules/`.
pub fn clear(worktree: &LinkedWorktree, clearance: Clearance) -> Outcome {
    let wt = &worktree.path;
    let Clearance {
        top_level,
        stranded,
    } = clearance;
    // Emptied by hand: the working-tree half of `submodule deinit -f`, and only that
    // half. deinit also unsets submodule.<name>.url/active/update, and that config is
    // shared by every worktree: run in a linked worktree it de-registers the submodules
    // of the main checkout (measured 2026-09-27).
    for sub in &top_level {
        empty_directory(&wt.join(sub))?;
    }
    if !top_level.is_empty() {
        say(
            Sink::Stdout,
            format_args!(
                "emptied {} submodule(s) in {} (all clean, all pushed)",
                top_level.len(),
                wt.display()
            ),
        );
    }
    // The same for a stranded one: git cannot run in it, and every file in it was just
    // proven to be in its recorded commit.
    for sub in &stranded {
        empty_directory(&wt.join(sub))?;
        say(
            Sink::Stdout,
            format_args!(
                "emptied stranded submodule {} (its files match {}'s recorded commit)",
                sub.display(),
                sub.display()
            ),
        );
    }

    // Never clear modules/ under a submodule that is still populated: that is the shape
    // nothing can inspect or remove afterwards.
    let modules = worktree.gitdir.join("modules");
    let remaining = submodules::populated(wt)?;
    if !remaining.is_empty() {
        let list: Vec<_> = remaining
            .iter()
            .map(|sub| sub.display().to_string())
            .collect();
        return Err(Stop::refused(format!(
            "still populated after emptying, so {} is NOT cleared (clearing it would strand them):\n{}",
            modules.display(),
            list.join("\n")
        )));
    }
    if paths::is_dir(&modules)? {
        fs::remove_dir_all(&modules).map_err(|error| {
            Stop::refused(format!("cannot delete {}: {error}", modules.display()))
        })?;
        say(
            Sink::Stdout,
            format_args!(
                "cleared {} (submodule clones, all pushed; git refuses removal while it exists)",
                modules.display()
            ),
        );
    }
    Ok(())
}

fn status(dir: &Path) -> Result<Vec<u8>, CannotTell> {
    Git::at(dir, ["status", "--porcelain", "--ignore-submodules=all"]).probe(dir)
}

fn empty_directory(dir: &Path) -> Outcome {
    let failed = |path: &Path, error: std::io::Error| {
        Stop::refused(format!("cannot delete {}: {error}", path.display()))
    };
    for entry in fs::read_dir(dir).map_err(|error| failed(dir, error))? {
        let entry = entry.map_err(|error| failed(dir, error))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| failed(&path, error))?;
        let removed = if kind.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removed.map_err(|error| failed(&path, error))?;
    }
    Ok(())
}

/// A stranded submodule's gitfile leads nowhere. Its recorded commit, read from
/// another clone of the same submodule, is what its files are compared against. Only
/// a top-level one can be traced to such a clone.
fn verify_top_level_stranded(
    worktree: &LinkedWorktree,
    sub: &Path,
    subs: &[PathBuf],
    head: &str,
) -> Outcome {
    if nested_in(sub, subs) {
        return Err(Stop::refused(format!(
            "stranded submodule {} sits inside a live one; cannot trace its clone — inspect it by hand",
            sub.display()
        )));
    }
    let name = submodules::name_at(&worktree.common, head, sub)?.ok_or_else(|| {
        Stop::refused(format!(
            "stranded submodule {} is not in {}'s .gitmodules",
            sub.display(),
            worktree.path.display()
        ))
    })?;
    let commit = submodules::recorded_commit(&worktree.path, sub)?.ok_or_else(|| {
        CannotTell::new(format!(
            "worktree: cannot inspect {}: {} has no recorded commit",
            worktree.path.display(),
            sub.display()
        ))
    })?;
    // The main checkout's clone first; then the worktree's own, which is the only one
    // left once main has retired the submodule.
    let candidates = [
        worktree.common.join("modules").join(&name),
        worktree.gitdir.join("modules").join(&name),
    ];
    let clone = submodules::clone_with(&commit, &candidates)?
        .ok_or_else(|| no_clone_for(worktree, sub, &name, &commit, head, &candidates))?;
    verify_stranded(&worktree.path.join(sub), &clone, &commit, sub)
}

/// Proves a stranded submodule's files hold nothing its recorded commit does not, by
/// comparing the directory against that commit through a throwaway index. Ignored
/// files go, as `submodule deinit -f` would take them; a modified or untracked file
/// refuses. Nested submodules are checked the same way, recursively, and a nested one
/// no clone can answer for stops at exit 3.
fn verify_stranded(dir: &Path, clone: &Path, commit: &str, label: &Path) -> Outcome {
    let index = TempFile::create().map_err(|error| cannot_read(label, &error))?;
    Git::in_clone(clone, ["read-tree", commit])
        .index_file(index.path())
        .probe(label)?;
    let (git_dir, work_tree) = (flag("--git-dir=", clone), flag("--work-tree=", dir));
    let args: [&OsStr; 6] = [
        &git_dir,
        &work_tree,
        OsStr::new("status"),
        OsStr::new("--porcelain"),
        OsStr::new("--untracked-files=all"),
        OsStr::new("--ignore-submodules=all"),
    ];
    let diff = Git::new(args).index_file(index.path()).probe(label)?;
    if !diff.is_empty() {
        return Err(Stop::refused(format!(
            "stranded submodule {} differs from its recorded commit {} — keep what you need from it first:\n{}",
            label.display(),
            short(commit),
            shown(&diff)
        )));
    }

    for nested in submodules::gitlinks_at(clone, commit, label)? {
        let nested_dir = dir.join(&nested.path);
        if !submodules::is_populated(&nested_dir)? {
            continue;
        }
        let nested_label = label.join(&nested.path);
        let name = submodules::name_at(clone, commit, &nested.path)?.ok_or_else(|| {
            Stop::refused(format!(
                "stranded submodule {} is not in {}'s .gitmodules at {}",
                nested_label.display(),
                label.display(),
                short(commit)
            ))
        })?;
        let candidate = clone.join("modules").join(&name);
        let nested_clone = submodules::clone_with(&nested.commit, std::slice::from_ref(&candidate))?
            .ok_or_else(|| {
                let base = Git::in_clone(clone, ["config", "--get", "remote.origin.url"])
                    .answer(clone)
                    .ok()
                    .flatten()
                    .map(|url| trimmed(url).to_string_lossy().into_owned());
                CannotTell::new(format!(
                    "worktree: cannot verify stranded submodule {}: no clone holds its recorded commit {}. Looked in:\n\
                     \x20         {}\n\
                     \x20         Clone it where the guard looks, then retry:\n{}",
                    nested_label.display(),
                    short(&nested.commit),
                    candidate.display(),
                    submodules::clone_advice(clone, commit, &name, base.as_deref(), &candidate)
                ))
            })?;
        verify_stranded(&nested_dir, &nested_clone, &nested.commit, &nested_label)?;
    }
    Ok(())
}

/// No clone anywhere can answer for a stranded submodule: says how to get one, in a
/// way that works whether or not the main checkout still declares it. Stops at exit 3,
/// never `--force` past it: its files may exist nowhere else.
fn no_clone_for(
    worktree: &LinkedWorktree,
    sub: &Path,
    name: &OsStr,
    commit: &str,
    head: &str,
    looked_in: &[PathBuf],
) -> CannotTell {
    let main = &worktree.main_checkout;
    let mut report = vec![format!(
        "worktree: cannot verify stranded submodule {}: no clone holds its recorded commit {}. Looked in:",
        sub.display(),
        short(commit)
    )];
    report.extend(
        looked_in
            .iter()
            .map(|place| format!("          {}", place.display())),
    );
    let mut key = OsString::from("submodule.");
    key.push(name);
    key.push(".path");
    let args: [&OsStr; 4] = [
        OsStr::new("config"),
        OsStr::new("--file=.gitmodules"),
        OsStr::new("--get"),
        &key,
    ];
    if Git::at(main, args).succeeds(main).unwrap_or(false) {
        report.push(format!(
            "          Initialise the submodule in the main checkout (git -C {} submodule update --init --recursive), then retry.",
            main.display()
        ));
    } else {
        let base = Git::at(main, ["remote", "get-url", "origin"])
            .answer(main)
            .ok()
            .flatten()
            .map(|url| trimmed(url).to_string_lossy().into_owned());
        let target = worktree.common.join("modules").join(name);
        report.push("          The main checkout no longer declares it. Clone it where the guard looks, then retry:".to_owned());
        report.push(submodules::clone_advice(
            &worktree.common,
            head,
            name,
            base.as_deref(),
            &target,
        ));
    }
    CannotTell::new(report.join("\n"))
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
