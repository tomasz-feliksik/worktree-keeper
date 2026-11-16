//! May this worktree be removed? Removal destroys two things nothing else keeps:
//! the worktree's uncommitted changes, and its submodule clones. Each worktree gets
//! its own under `<gitdir>/modules`, and a commit made in one exists nowhere else
//! until pushed. The worktree's own commits live in the shared repository, and whether
//! its branch is deleted is the removing tool's decision, unless HEAD is detached: then
//! only HEAD holds them, and it goes with the worktree.
//!
//! Then clears `<gitdir>/modules`: git refuses `worktree remove` on that directory's
//! mere existence, and nothing else ever deletes it. That refusal is reached only
//! without `--force`, which goes straight past it. So the value of this guard is the
//! unpushed check, not getting past git. Never "simplify" it into `--force`: that
//! deletes submodule clones unchecked.
//!
//! `--force` accepts losing the worktree's own uncommitted changes and the commits on its
//! detached HEAD. Submodule changes and unpushed submodule commits are refused regardless.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::git::{Git, flag, records, shown, trimmed, trimmed_path};
use crate::in_use;
use crate::outcome::{CannotTell, Outcome, RefusalKind, Stop};
use crate::paths::{self, cannot_read};
use crate::say::{Sink, say};
use crate::submodules::{self, nested_in};
use crate::temp::TempFile;
use crate::worktree::LinkedWorktree;

/// Whether the worktree's own uncommitted changes, and commits only its HEAD holds, may
/// be lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnChanges {
    MustBeClean,
    MayBeLost,
}

/// Whether a process running inside the worktree stops the removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningProcesses {
    Refuse,
    Ignore,
}

/// What the caller has accepted. `--force` does not cover running processes: it accepts
/// losing changes already made, not pulling a directory from under a working process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub own_changes: OwnChanges,
    pub running: RunningProcesses,
}

impl Rules {
    #[must_use]
    pub fn from_flags(force: bool, ignore_in_use: bool) -> Self {
        Self {
            own_changes: if force {
                OwnChanges::MayBeLost
            } else {
                OwnChanges::MustBeClean
            },
            running: if ignore_in_use {
                RunningProcesses::Ignore
            } else {
                RunningProcesses::Refuse
            },
        }
    }
}

/// Proof that every file and clone was found disposable. Only [`assess`] can make
/// one, and [`clear`] needs one, so nothing can be emptied before it was checked.
#[derive(Debug)]
#[must_use]
pub struct Clearance {
    top_level: Vec<PathBuf>,
    stranded: Vec<PathBuf>,
    /// Every live populated submodule, nested ones included, each after the one it is in.
    live: Vec<PathBuf>,
}

/// `guard`: assess, then clear. Never removes the worktree.
pub fn run(path: Option<PathBuf>, rules: Rules) -> Outcome {
    let path = path.map_or_else(env::current_dir, Ok).map_err(|error| {
        CannotTell::new(format!(
            "worktree: cannot read the current directory: {error}"
        ))
    })?;
    // Kept as cleared: the tool that removes the worktree next deletes what was set aside.
    prepare(&path, rules).map(drop)
}

/// Assess, then clear, and say so: what `guard` does, and what `remove` does before it
/// asks git. The record lets `remove` put everything back if git then refuses.
pub fn prepare(path: &Path, rules: Rules) -> Outcome<(LinkedWorktree, Cleared)> {
    let worktree = LinkedWorktree::resolve(path)?;
    let clearance = assess(&worktree, rules)?;
    if rules.running == RunningProcesses::Ignore {
        report_left_running(&worktree.path);
    }
    let cleared = clear(&worktree, clearance)?;
    say(
        Sink::Stdout,
        format_args!("{} may be removed", worktree.path.display()),
    );
    Ok((worktree, cleared))
}

/// What [`clear`] changed, so that a removal git refuses can be undone.
#[derive(Debug)]
#[must_use]
pub struct Cleared {
    worktree: PathBuf,
    /// `<gitdir>/modules`, if it was set aside.
    modules: Option<PathBuf>,
    /// Each live submodule emptied, nested ones included, parent first, with the bytes of
    /// its `.git` file.
    gitfiles: Vec<(PathBuf, Vec<u8>)>,
    stranded: Vec<PathBuf>,
}

impl Cleared {
    /// Puts back what [`clear`] set aside and emptied: the clones where they were, then
    /// each submodule's `.git` file as it was and its files from its own HEAD, parent
    /// first, so a nested one finds its directory. Nothing in them is lost: each was proven
    /// clean, and its index, HEAD and branch live in the clone, which never moved off disk.
    /// A stranded submodule stays empty: its clone was gone before, and its files were
    /// proven to be its recorded commit.
    pub fn restore(self) -> Outcome {
        if let Some(modules) = &self.modules {
            fs::rename(aside_of(modules), modules).map_err(|error| {
                CannotTell::new(format!(
                    "worktree: cannot put {} back from {}: {error}",
                    modules.display(),
                    aside_of(modules).display()
                ))
            })?;
        }
        for (sub, gitfile) in &self.gitfiles {
            let dir = self.worktree.join(sub);
            fs::create_dir_all(&dir)
                .and_then(|()| fs::write(dir.join(".git"), gitfile))
                .map_err(|error| cannot_read(&dir, &error))?;
            // Each level by hand: recursing would reach a nested one before its .git file.
            Git::at(
                &dir,
                ["-c", "submodule.recurse=false", "reset", "--hard", "-q"],
            )
            .probe(&dir)?;
        }
        say(
            Sink::Stderr,
            format_args!(
                "put back {} submodule(s) in {}",
                self.gitfiles.len(),
                self.worktree.display()
            ),
        );
        for sub in &self.stranded {
            say(
                Sink::Stderr,
                format_args!(
                    "stranded submodule {} stays empty: its clone was already gone, and its files were its recorded commit",
                    sub.display()
                ),
            );
        }
        Ok(())
    }
}

/// Where [`clear`] sets a worktree's submodule clones aside: beside `modules`, under a name
/// git does not refuse removal on, inside the git directory that `git worktree remove`
/// deletes. A rename in one directory, so it cannot cross a filesystem.
fn aside_of(modules: &Path) -> PathBuf {
    modules.with_file_name(submodules::SET_ASIDE)
}

/// The record of what `--ignore-in-use` skipped, read now because a process stops
/// matching once its directory is gone. Only a report: the caller has already accepted
/// these processes, so failing to list them does not stop the removal.
fn report_left_running(wt: &Path) {
    match in_use::holders(wt) {
        Ok(holders) => {
            for holder in holders {
                say(Sink::Stdout, format_args!("still running: {holder}"));
            }
        }
        Err(_) => say(
            Sink::Stderr,
            format_args!("could not tell what is still running in {}", wt.display()),
        ),
    }
}

/// Every check, changing nothing on disk.
pub fn assess(worktree: &LinkedWorktree, rules: Rules) -> Outcome<Clearance> {
    let wt = &worktree.path;
    if let Some(reason) = lock_reason(worktree)? {
        return Err(Stop::refused_as(
            RefusalKind::Locked,
            format!(
                "{} is locked ({}) — release it with 'worktree unlock {}' once nothing depends on it",
                wt.display(),
                if reason.is_empty() {
                    "no reason given"
                } else {
                    &reason
                },
                wt.display()
            ),
        ));
    }
    if rules.running == RunningProcesses::Refuse {
        let holders = in_use::holders(wt)?;
        if !holders.is_empty() {
            let named: Vec<_> = holders.iter().map(ToString::to_string).collect();
            return Err(Stop::refused_as(
                RefusalKind::InUse,
                format!(
                    "{} is in use by {} — stop them, or run this from outside the worktree if the tool closes its own panes, or pass --ignore-in-use if you own them",
                    wt.display(),
                    named.join(", ")
                ),
            ));
        }
    }
    if rules.own_changes == OwnChanges::MustBeClean {
        refuse_own_work(worktree)?;
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
            return Err(Stop::refused_as(
                RefusalKind::DirtySubmodule,
                format!(
                    "submodule {} in {} has uncommitted changes — commit and push them there first:\n{}",
                    sub.display(),
                    wt.display(),
                    shown(&dirty)
                ),
            ));
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
            return Err(Stop::refused_as(
                RefusalKind::UnpushedSubmodule,
                format!(
                    "{} holds commits no remote has — a submodule clone that exists only in this worktree; push them first:\n{}",
                    clone.display(),
                    shown(&pending)
                ),
            ));
        }
    }

    // After the submodule checks: an unpushed submodule commit is a pointer change too, and
    // its own refusal, which --force does not cover, is the one to report.
    if rules.own_changes == OwnChanges::MustBeClean {
        refuse_pointer_changes(wt)?;
    }

    let top_level = live
        .iter()
        .filter(|sub| !nested_in(sub, &subs))
        .cloned()
        .collect();
    Ok(Clearance {
        top_level,
        stranded,
        live,
    })
}

/// What `--force` accepts losing: the worktree's uncommitted changes, and the commits only
/// its detached HEAD holds.
fn refuse_own_work(worktree: &LinkedWorktree) -> Outcome {
    let wt = &worktree.path;
    let dirty = status(wt)?;
    if !dirty.is_empty() {
        return Err(Stop::refused_as(
            RefusalKind::Dirty,
            format!(
                "{} has uncommitted changes — commit them, or discard them with 'worktree remove --force {}':\n{}",
                wt.display(),
                wt.display(),
                shown(&dirty)
            ),
        ));
    }
    let only_here = commits_only_head_holds(&worktree.gitdir)?;
    if !only_here.is_empty() {
        return Err(Stop::refused_as(
            RefusalKind::DetachedCommits,
            format!(
                "{} is on a detached HEAD with commits no branch, tag or remote holds — keep them with 'git branch <name>', or discard them with 'worktree remove --force {}':\n{}",
                wt.display(),
                wt.display(),
                shown(&only_here)
            ),
        ));
    }
    Ok(())
}

/// What `--force` accepts losing, besides `refuse_own_work`'s: a gitlink moved and not
/// committed.
fn refuse_pointer_changes(wt: &Path) -> Outcome {
    let moved = pointer_changes(wt)?;
    if !moved.is_empty() {
        return Err(Stop::refused_as(
            RefusalKind::SubmodulePointer,
            format!(
                "{} has an uncommitted submodule pointer change — commit the bump, or discard it with 'worktree remove --force {}':\n{}",
                wt.display(),
                wt.display(),
                moved.join("\n")
            ),
        ));
    }
    Ok(())
}

/// The reason in a `git worktree lock`, if the worktree is locked. It can be empty.
pub fn lock_reason(worktree: &LinkedWorktree) -> Result<Option<String>, CannotTell> {
    let file = worktree.gitdir.join("locked");
    if !paths::exists(&file)? {
        return Ok(None);
    }
    let reason = fs::read(&file).map_err(|error| cannot_read(&file, &error))?;
    Ok(Some(shown(&reason)))
}

/// With every clone proven disposable: empties the submodules and clears `modules/`.
pub fn clear(worktree: &LinkedWorktree, clearance: Clearance) -> Outcome<Cleared> {
    let wt = &worktree.path;
    let Clearance {
        top_level,
        stranded,
        live,
    } = clearance;
    let modules = worktree.gitdir.join("modules");
    let aside = aside_of(&modules);
    if paths::exists(&aside)? && paths::exists(&modules)? {
        return Err(Stop::refused(format!(
            "{} already holds submodule clones an earlier guard set aside, so nothing was cleared — remove the worktree, or move them back to {} by hand",
            aside.display(),
            modules.display()
        )));
    }
    let mut gitfiles = Vec::new();
    for sub in &live {
        let gitfile = wt.join(sub).join(".git");
        let bytes = fs::read(&gitfile).map_err(|error| cannot_read(&gitfile, &error))?;
        gitfiles.push((sub.clone(), bytes));
    }
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
    let remaining = submodules::populated(wt)?;
    if !remaining.is_empty() {
        let list: Vec<_> = remaining
            .iter()
            .map(|sub| sub.display().to_string())
            .collect();
        return Err(Stop::refused_as(
            RefusalKind::StillPopulated,
            format!(
                "still populated after emptying, so {} is NOT cleared (clearing it would strand them):\n{}",
                modules.display(),
                list.join("\n")
            ),
        ));
    }
    // Set aside, not deleted: git may still refuse the removal, and then they go back.
    let set_aside = paths::is_dir(&modules)?;
    if set_aside {
        fs::rename(&modules, &aside).map_err(|error| {
            Stop::refused(format!(
                "cannot move {} to {}: {error}",
                modules.display(),
                aside.display()
            ))
        })?;
        say(
            Sink::Stdout,
            format_args!(
                "set {} aside as {} (submodule clones, all pushed; git refuses removal while modules exists, and deletes the copy with the worktree)",
                modules.display(),
                aside.display()
            ),
        );
    }
    Ok(Cleared {
        worktree: wt.clone(),
        modules: set_aside.then_some(modules),
        gitfiles,
        stranded,
    })
}

/// `--no-optional-locks`: a plain `git status` refreshes the index when it can, and a
/// question must not write. `check` and `status` are built on this.
///
/// `--untracked-files`, because `status.showUntrackedFiles` would otherwise decide what
/// counts as safe to delete.
pub fn status(dir: &Path) -> Result<Vec<u8>, CannotTell> {
    let mut found = Git::at(
        dir,
        [
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    )
    .probe(dir)?;
    found.extend(hidden_edits(dir)?);
    Ok(found)
}

/// Gitlinks that differ from the commit HEAD records, staged or not, one line each naming
/// the submodule and both commits. `status` passes `--ignore-submodules=all`, which hides
/// these too, and the submodule checks look inside the submodule, where nothing is wrong
/// when it sits at a pushed commit: so without this, nothing reported the pointer
/// (worktree-keeper-jwv). `=dirty` reports a changed commit and leaves the content inside a
/// submodule to the dirty-submodule check.
fn pointer_changes(dir: &Path) -> Result<Vec<String>, CannotTell> {
    let listing = Git::at(
        dir,
        [
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=dirty",
        ],
    )
    .probe(dir)?;
    let mut moved = Vec::new();
    let mut entries = records(&listing);
    while let Some(entry) = entries.next() {
        // A rename's record is followed by its original path, which is no entry.
        if entry.starts_with(b"2 ") {
            entries.next();
        }
        let Some(change) = PointerChange::parse(entry) else {
            continue;
        };
        let now = if change.recorded == change.staged {
            shown(&Git::at(&dir.join(change.path), ["rev-parse", "HEAD"]).probe(dir)?)
        } else {
            change.staged.to_owned()
        };
        moved.push(format!(
            "submodule {} points at {} but {} is recorded",
            change.path.display(),
            short(&now),
            short(change.recorded)
        ));
    }
    Ok(moved)
}

/// A `status --porcelain=v2` entry whose submodule field says its commit changed.
struct PointerChange<'a> {
    path: &'a Path,
    /// The commit HEAD records.
    recorded: &'a str,
    /// The commit the index records.
    staged: &'a str,
}

impl<'a> PointerChange<'a> {
    /// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` and `2 … <X><score> <path>`, where
    /// `<sub>` is `S<c><m><u>` for a submodule. `<c>` is `C` only when the checked-out commit
    /// differs from the index (measured on 2.56: a staged bump reads `S...`), so a staged one
    /// shows as `<hH>` differing from `<hI>`.
    fn parse(entry: &'a [u8]) -> Option<Self> {
        let fields = if entry.starts_with(b"1 ") {
            9
        } else if entry.starts_with(b"2 ") {
            10
        } else {
            return None;
        };
        let mut parts = entry.splitn(fields, |byte| *byte == b' ');
        let sub = parts.nth(2)?;
        let recorded = std::str::from_utf8(parts.nth(3)?).ok()?;
        let staged = std::str::from_utf8(parts.next()?).ok()?;
        let path = Path::new(OsStr::from_bytes(parts.last()?));
        let unstaged = sub.get(1) == Some(&b'C');
        if !sub.starts_with(b"S") || !(unstaged || recorded != staged) {
            return None;
        }
        Some(Self {
            path,
            recorded,
            staged,
        })
    }
}

/// Edits to files flagged skip-worktree or assume-unchanged, which `git status` does not
/// look at. Compared through a throwaway copy of the index with the flags cleared, so that
/// git makes the comparison and a question leaves the real index as it was.
///
/// Only files on disk: under a sparse checkout, a flagged file is normally absent.
fn hidden_edits(dir: &Path) -> Result<Vec<u8>, CannotTell> {
    let listing = Git::at(dir, ["ls-files", "-v", "-z"]).probe(dir)?;
    let flagged: Vec<&OsStr> = records(&listing)
        .filter_map(flagged_path)
        .filter(|path| on_disk(&dir.join(path)))
        .collect();
    if flagged.is_empty() {
        return Ok(Vec::new());
    }
    let index = TempFile::create().map_err(|error| cannot_read(dir, &error))?;
    let real_index = trimmed_path(
        Git::at(
            dir,
            ["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )
        .probe(dir)?,
    );
    fs::copy(&real_index, index.path()).map_err(|error| cannot_read(&real_index, &error))?;

    // One run per flag: given both, `update-index` applies only the first it handles.
    for flag in ["--no-skip-worktree", "--no-assume-unchanged"] {
        let mut clear: Vec<&OsStr> = ["update-index", flag, "--"].map(OsStr::new).to_vec();
        clear.extend(&flagged);
        Git::at(dir, clear).index_file(index.path()).probe(dir)?;
    }

    let mut diff: Vec<&OsStr> = [
        "--no-optional-locks",
        "--literal-pathspecs",
        "diff",
        "--name-only",
        "--ignore-submodules=all",
        "--",
    ]
    .map(OsStr::new)
    .to_vec();
    diff.extend(&flagged);
    let edited = Git::at(dir, diff).index_file(index.path()).probe(dir)?;

    let mut report = Vec::new();
    for line in edited
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        report.extend_from_slice(b"hidden: ");
        report.extend_from_slice(line);
        report.push(b'\n');
    }
    Ok(report)
}

/// The path in an `ls-files -v` record if it is flagged: `S` is skip-worktree, and a
/// lowercase tag is assume-unchanged.
fn flagged_path(record: &[u8]) -> Option<&OsStr> {
    let (tag, rest) = record.split_first()?;
    let path = rest.strip_prefix(b" ")?;
    (*tag == b'S' || tag.is_ascii_lowercase()).then(|| OsStr::from_bytes(path))
}

/// Anything but a definite "not there" counts as there, for git to make the comparison.
fn on_disk(path: &Path) -> bool {
    !matches!(
        fs::symlink_metadata(path),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
    )
}

/// Commits a detached HEAD holds that nothing outside this worktree does, given its
/// private git directory, which outlives a deleted worktree. Not `--all`, as for a submodule
/// clone: that counts HEAD itself, and refs such as `refs/bisect` that are this worktree's
/// own. A commit held only by some other kind of ref is refused, which `--force` or a branch
/// settles.
pub fn commits_only_head_holds(gitdir: &Path) -> Result<Vec<u8>, CannotTell> {
    let branch = Git::in_clone(gitdir, ["branch", "--show-current"]).probe(gitdir)?;
    if !trimmed(branch).is_empty() {
        return Ok(Vec::new());
    }
    Git::in_clone(
        gitdir,
        [
            "log",
            "--oneline",
            "HEAD",
            "--not",
            "--branches",
            "--tags",
            "--remotes",
        ],
    )
    .probe(gitdir)
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
        return Err(Stop::refused_as(
            RefusalKind::StrandedUntraceable,
            format!(
                "stranded submodule {} sits inside a live one; cannot trace its clone — inspect it by hand",
                sub.display()
            ),
        ));
    }
    let name = submodules::name_at(&worktree.common, head, sub)?.ok_or_else(|| {
        Stop::refused_as(
            RefusalKind::StrandedUntraceable,
            format!(
                "stranded submodule {} is not in {}'s .gitmodules",
                sub.display(),
                worktree.path.display()
            ),
        )
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
        return Err(Stop::refused_as(
            RefusalKind::StrandedDiffers,
            format!(
                "stranded submodule {} differs from its recorded commit {} — keep what you need from it first:\n{}",
                label.display(),
                short(commit),
                shown(&diff)
            ),
        ));
    }

    for nested in submodules::gitlinks_at(clone, commit, label)? {
        let nested_dir = dir.join(&nested.path);
        if !submodules::is_populated(&nested_dir)? {
            continue;
        }
        let nested_label = label.join(&nested.path);
        let name = submodules::name_at(clone, commit, &nested.path)?.ok_or_else(|| {
            Stop::refused_as(
                RefusalKind::StrandedUntraceable,
                format!(
                    "stranded submodule {} is not in {}'s .gitmodules at {}",
                    nested_label.display(),
                    label.display(),
                    short(commit)
                ),
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn a_staged_file_is_never_a_pointer_change_though_its_objects_differ() {
        let entry = format!("1 M. N... 100644 100644 100644 {A} {B} file");
        assert!(PointerChange::parse(entry.as_bytes()).is_none());
    }

    #[test]
    fn a_staged_submodule_bump_is_a_pointer_change_naming_both_commits() {
        let entry = format!("1 M. S... 160000 160000 160000 {A} {B} lib/sub");
        let change = PointerChange::parse(entry.as_bytes()).expect("a pointer change");
        assert_eq!(
            (change.path, change.recorded, change.staged),
            (Path::new("lib/sub"), A, B)
        );
    }

    #[test]
    fn a_submodule_with_only_content_changed_is_no_pointer_change() {
        let entry = format!("1 .M S.M. 160000 160000 160000 {A} {A} lib/sub");
        assert!(PointerChange::parse(entry.as_bytes()).is_none());
    }

    #[test]
    fn a_commit_is_shortened_to_twelve_characters_when_it_has_them() {
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
        assert_eq!(short("0123"), "0123");
    }
}
