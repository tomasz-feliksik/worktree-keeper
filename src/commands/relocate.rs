//! `move`: relocates a linked worktree, submodules included, to where placement says it
//! belongs, and proves afterwards that nothing in it changed.
//!
//! `git worktree move` refuses a worktree whose submodules have clones under
//! `<gitdir>/modules` (measured on 2.56: uninitialised gitlinks alone do not stop it). Those
//! clones stay where they are, in the main repository; what goes stale is the two relative
//! pointers between each clone and its submodule directory, the submodule's `.git` file and
//! the clone's `core.worktree`. So such a worktree is renamed, git repairs its own two
//! links, and this rewrites the submodules' pointers, relative from their new place, as git
//! writes them.
//!
//! Every refusal comes before anything changes. Once the directory has moved, the only way
//! out is forward: a snapshot taken before and after has to match, and a mismatch, or an
//! after that cannot be taken, exits 3 and keeps both snapshots for a person to read. There
//! is no rollback: undoing a move can fail the same way.
//!
//! The snapshot reads only what git tracks. What it ignores is searched afterwards for the
//! old absolute path (`crate::stale`), and every entry still naming it is reported,
//! unchanged: whatever made it has to make it again.

use std::ffi::OsStr;
use std::fs;
use std::mem::ManuallyDrop;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::commands::check::{Format, Reason, SCHEMA_VERSION};
use crate::commands::guard;
use crate::git::{Git, flag, shown, trimmed_path};
use crate::in_use;
use crate::outcome::{CannotTell, Outcome, RefusalKind, Stop};
use crate::paths::{self, cannot_read};
use crate::placement;
use crate::say::{self, Sink, say};
use crate::stale::{self, Stale};
use crate::submodules;
use crate::temp::TempDir;
use crate::worktree::LinkedWorktree;

/// Where a worktree was and where it is now, and what git ignores there that still names
/// where it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Moved {
    from: String,
    to: String,
    stale: Vec<Stale>,
}

/// See docs/json-schema.md before changing what this describes.
#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    moved: Option<Moved>,
    reasons: &'a [Reason],
}

pub fn run(path: &Path, destination: Option<&Path>, format: Format) -> Outcome {
    let answer = relocate(path, destination);
    match format {
        Format::Json => {
            let reasons: Vec<Reason> = answer.as_ref().err().map(Reason::of).into_iter().collect();
            say::print_json(&Report {
                schema_version: SCHEMA_VERSION,
                moved: answer.as_ref().ok().cloned().flatten(),
                reasons: &reasons,
            })?;
        }
        Format::Text => match &answer {
            Ok(Some(moved)) => {
                say(
                    Sink::Stdout,
                    format_args!("moved {} to {}", moved.from, moved.to),
                );
                for stale in &moved.stale {
                    say(
                        Sink::Stdout,
                        format_args!(
                            "  {} still names the old path (in {}); move changed nothing in it, so remake it there",
                            stale.entry, stale.file
                        ),
                    );
                }
            }
            Ok(None) => say(
                Sink::Stdout,
                format_args!("{} is already where it belongs", path.display()),
            ),
            Err(_) => {}
        },
    }
    answer.map(drop)
}

/// The move itself; `None` when the worktree is already there.
fn relocate(path: &Path, destination: Option<&Path>) -> Outcome<Option<Moved>> {
    let worktree = LinkedWorktree::resolve(path)?;
    let from = &worktree.path;
    let to = if let Some(destination) = destination {
        absolute(destination)?
    } else {
        let name = from.file_name().ok_or_else(|| {
            CannotTell::new(format!(
                "worktree: {} has no directory name",
                from.display()
            ))
        })?;
        placement::dir_for(&worktree.common)?.join(name)
    };
    if is_same_place(from, &to)? {
        return Ok(None);
    }
    refuse_what_a_move_breaks(&worktree, &to)?;

    // Before anything changes: a worktree that cannot be read now could never be shown
    // unchanged afterwards.
    let before = Snapshot::take(from)?;
    let parent = to
        .parent()
        .ok_or_else(|| Stop::refused(format!("{} has no parent directory", to.display())))?;
    fs::create_dir_all(parent)
        .map_err(|error| Stop::refused(format!("cannot create {}: {error}", parent.display())))?;
    // Spelled from the parent's real path, so the relative pointers are computed between
    // real paths, as git computes them.
    let to = parent
        .canonicalize()
        .map_err(|error| cannot_read(parent, &error))?
        .join(to.file_name().unwrap_or(OsStr::new("")));
    relocate_worktree(&worktree, &to)?;
    verify(&before, &to)?;
    let stale = stale::entries_naming(&to, from).map_err(|cannot| {
        CannotTell::new(format!(
            "worktree: moved to {} and verified, but {cannot}",
            to.display()
        ))
    })?;
    if let Ok(dir) = placement::dir_for(&worktree.common)
        && from.parent() == Some(dir.as_path())
    {
        placement::remove_if_empty(&dir, Sink::Stderr);
    }
    Ok(Some(Moved {
        from: from.display().to_string(),
        to: to.display().to_string(),
        stale,
    }))
}

/// An absolute spelling of a destination, as the caller meant it from where they stand.
fn absolute(destination: &Path) -> Outcome<PathBuf> {
    std::path::absolute(destination).map_err(|error| {
        Stop::refused(format!(
            "cannot tell where {} is: {error}",
            destination.display()
        ))
    })
}

/// Whether `to` is where the worktree already is.
fn is_same_place(from: &Path, to: &Path) -> Result<bool, CannotTell> {
    if !paths::exists(to)? {
        return Ok(false);
    }
    let to = to.canonicalize().map_err(|error| cannot_read(to, &error))?;
    Ok(to == from)
}

/// What a move would break, each refused with nothing changed. A process in the worktree
/// would find every absolute path it holds gone; a lock is someone's claim on the
/// worktree where it is.
fn refuse_what_a_move_breaks(worktree: &LinkedWorktree, to: &Path) -> Outcome {
    let wt = &worktree.path;
    if let Some(reason) = guard::lock_reason(worktree)? {
        return Err(Stop::refused_as(
            RefusalKind::Locked,
            format!(
                "{} is locked ({}), so it was not moved — release it with 'worktree unlock {}' once nothing depends on where it is",
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
    let holders = in_use::holders(wt)?;
    if !holders.is_empty() {
        let named: Vec<_> = holders.iter().map(ToString::to_string).collect();
        return Err(Stop::refused_as(
            RefusalKind::InUse,
            format!(
                "{} is in use by {}, so it was not moved — a process there would lose its paths; stop them first",
                wt.display(),
                named.join(", ")
            ),
        ));
    }
    if paths::exists(to)? {
        return Err(Stop::refused_as(
            RefusalKind::DestinationExists,
            format!(
                "{} already exists, so {} was not moved",
                to.display(),
                wt.display()
            ),
        ));
    }
    Ok(())
}

/// Moves the directory and repairs every pointer into and out of it. `to` does not exist.
fn relocate_worktree(worktree: &LinkedWorktree, to: &Path) -> Outcome {
    let (from, main) = (&worktree.path, &worktree.main_checkout);
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Stop::refused(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    if !paths::is_dir(&worktree.gitdir.join("modules"))? {
        let args: [&OsStr; 4] = [
            OsStr::new("worktree"),
            OsStr::new("move"),
            from.as_os_str(),
            to.as_os_str(),
        ];
        let moved = Git::at(main, args).act(Sink::Stderr).unwrap_or(false);
        return if moved {
            Ok(())
        } else {
            Err(Stop::refused(format!(
                "{} was not moved: git said why, above",
                from.display()
            )))
        };
    }

    // Each clone, read while the pointers to it still resolve.
    let mut clones = Vec::new();
    for sub in submodules::populated(from)? {
        let dir = from.join(&sub);
        let clone = Git::at(&dir, ["rev-parse", "--absolute-git-dir"]).probe(&dir)?;
        clones.push((sub, trimmed_path(clone)));
    }
    fs::rename(from, to).map_err(|error| {
        Stop::refused(format!(
            "cannot move {} to {}: {error}",
            from.display(),
            to.display()
        ))
    })?;
    // From here on something has changed, so a failure is "cannot tell", never "refused".
    Git::at(
        main,
        [OsStr::new("worktree"), OsStr::new("repair"), to.as_os_str()],
    )
    .probe(to)?;
    for (sub, clone) in &clones {
        repoint_submodule(&to.join(sub), clone)?;
    }
    Ok(())
}

/// Writes both pointers between a submodule directory and its clone, relative from where
/// each is held, as `git submodule` writes them.
fn repoint_submodule(dir: &Path, clone: &Path) -> Result<(), CannotTell> {
    let gitfile = dir.join(".git");
    let mut pointer = b"gitdir: ".to_vec();
    pointer.extend_from_slice(relative(dir, clone).as_os_str().as_encoded_bytes());
    pointer.push(b'\n');
    fs::write(&gitfile, pointer).map_err(|error| cannot_read(&gitfile, &error))?;
    let config = clone.join("config");
    let args = [
        OsStr::new("config").to_owned(),
        flag("--file=", &config),
        OsStr::new("core.worktree").to_owned(),
        relative(clone, dir).into_os_string(),
    ];
    Git::new(args).probe(clone).map(drop)
}

/// The path from directory `from` to `to`, both absolute and real.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in shared..from.len() {
        path.push("..");
    }
    for component in &to[shared..] {
        path.push(component);
    }
    path
}

/// What a move must not change, read through git: the worktree's HEAD, branch and status,
/// and each populated submodule's HEAD, status and stashes. Statuses list paths relative
/// to their top level, so a correct move leaves them byte for byte the same.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    entries: Vec<(String, String)>,
}

impl Snapshot {
    fn take(worktree: &Path) -> Result<Self, CannotTell> {
        let mut entries = Vec::new();
        read_into(&mut entries, "", worktree)?;
        for sub in submodules::populated(worktree)? {
            let dir = worktree.join(&sub);
            if submodules::is_stranded(&dir)? {
                return Err(CannotTell::new(format!(
                    "worktree: cannot read submodule {} in {}: its clone is gone, so nothing could show it unchanged after a move",
                    sub.display(),
                    worktree.display()
                )));
            }
            read_into(&mut entries, &format!("{} ", sub.display()), &dir)?;
        }
        Ok(Self { entries })
    }

    /// What differs between two snapshots, each named by its entry: a value that changed,
    /// or an entry only one of them has, such as a submodule no longer populated.
    fn differences(&self, other: &Self) -> Vec<String> {
        let value_in = |snapshot: &Self, key: &str| {
            snapshot
                .entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        let mut found = Vec::new();
        for (key, value) in &self.entries {
            match value_in(other, key) {
                Some(now) if now == *value => {}
                Some(_) => found.push(format!("{key} differs")),
                None => found.push(format!("{key} is gone")),
            }
        }
        for (key, _) in &other.entries {
            if value_in(self, key).is_none() {
                found.push(format!("{key} is new"));
            }
        }
        found
    }

    fn render(&self) -> String {
        let mut text = String::new();
        for (key, value) in &self.entries {
            for part in ["== ", key, "\n", value, "\n"] {
                text.push_str(part);
            }
        }
        text
    }
}

fn read_into(
    entries: &mut Vec<(String, String)>,
    label: &str,
    dir: &Path,
) -> Result<(), CannotTell> {
    let ask = |args: &[&str]| -> Result<String, CannotTell> {
        Git::at(dir, args).probe(dir).map(|output| shown(&output))
    };
    entries.push((format!("{label}HEAD"), ask(&["rev-parse", "HEAD"])?));
    entries.push((
        format!("{label}branch"),
        ask(&["branch", "--show-current"])?,
    ));
    entries.push((
        format!("{label}status"),
        ask(&[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ])?,
    ));
    entries.push((format!("{label}stashes"), ask(&["stash", "list"])?));
    Ok(())
}

/// Compares a fresh snapshot of the moved worktree with the one taken before. Both are kept
/// on disk when they disagree, or when the second cannot be taken.
fn verify(before: &Snapshot, moved: &Path) -> Outcome {
    let after = Snapshot::take(moved);
    if after.as_ref() == Ok(before) {
        return Ok(());
    }
    let kept = keep(before, after.as_ref().ok())?;
    let problem = match &after {
        Ok(after) => before.differences(after).join("; "),
        Err(cannot) => format!("it cannot be read: {cannot}"),
    };
    Err(CannotTell::new(format!(
        "worktree: moved to {}, but cannot show that nothing in it changed — {problem}. It was NOT moved back; compare the snapshots:\n  snapshot before: {}\n  snapshot after: {}",
        moved.display(),
        kept.join("before").display(),
        kept.join("after").display()
    ))
    .into())
}

/// Writes the snapshots to a directory that outlives the command, for a person to read.
fn keep(before: &Snapshot, after: Option<&Snapshot>) -> Result<PathBuf, CannotTell> {
    let dir = TempDir::create().map_err(|error| {
        CannotTell::new(format!("worktree: cannot keep the snapshots: {error}"))
    })?;
    let dir = ManuallyDrop::new(dir).path().to_path_buf();
    let after = after.map_or_else(|| "(could not be taken)\n".to_owned(), Snapshot::render);
    for (name, text) in [("before", before.render()), ("after", after)] {
        let file = dir.join(name);
        fs::write(&file, text).map_err(|error| cannot_read(&file, &error))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::testutil::{Scratch, worktree_with_nested_submodules};

    fn git_config(file: &Path, args: &[&str]) -> String {
        let ran = Command::new("git")
            .arg("config")
            .arg("--file")
            .arg(file)
            .args(args)
            .output()
            .unwrap();
        assert!(ran.status.success(), "git config {args:?}");
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    }

    #[test]
    fn a_relative_path_climbs_to_the_shared_prefix_then_descends() {
        assert_eq!(
            relative(Path::new("/a/b/c"), Path::new("/a/x/y")),
            Path::new("../../x/y")
        );
        assert_eq!(
            relative(Path::new("/a/b"), Path::new("/a/b/c")),
            Path::new("c")
        );
    }

    /// The text of the snapshot a failed verification kept, by the label in its report.
    fn kept(report: &str, label: &str) -> String {
        let prefix = format!("snapshot {label}: ");
        let path = report
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| {
                panic!("the report does not say where the {label} snapshot is: {report}")
            });
        fs::read_to_string(path).unwrap()
    }

    // A change to what a worktree holds is named, and both snapshots show it.
    #[test]
    fn verification_names_what_changed_and_keeps_both_snapshots() {
        let scratch = Scratch::new();
        let wt = worktree_with_nested_submodules(scratch.path());
        let before = Snapshot::take(&wt).unwrap();
        fs::write(wt.join("sub/appeared"), "x").unwrap();

        let report = verify(&before, &wt).unwrap_err().to_string();

        assert!(report.contains("sub status differs"), "{report}");
        assert!(
            !report.contains("sub HEAD differs"),
            "an unchanged entry was named: {report}"
        );
        assert!(!report.contains("is gone"), "{report}");
        assert!(!kept(&report, "before").contains("appeared"));
        assert!(kept(&report, "after").contains("?? appeared"));
        assert!(kept(&report, "before").contains("== sub/nested HEAD"));
    }

    // A submodule that is no longer populated is named as gone, not passed over.
    #[test]
    fn verification_names_a_submodule_that_is_gone() {
        let scratch = Scratch::new();
        let wt = worktree_with_nested_submodules(scratch.path());
        let before = Snapshot::take(&wt).unwrap();
        fs::remove_dir_all(wt.join("sub/nested")).unwrap();
        fs::create_dir(wt.join("sub/nested")).unwrap();

        let report = verify(&before, &wt).unwrap_err().to_string();

        assert!(report.contains("sub/nested HEAD is gone"), "{report}");
        assert!(!report.contains("is new"), "{report}");
    }

    // AC6: the snapshot is the gate. A move that leaves one pointer stale — the submodule's
    // core.worktree, as if its rewrite were skipped — fails verification loudly, as could
    // not check, and says where both snapshots are kept.
    #[test]
    fn verification_fails_when_a_core_worktree_is_left_stale() {
        let scratch = Scratch::new();
        let old = worktree_with_nested_submodules(scratch.path());
        let worktree = LinkedWorktree::resolve(&old).unwrap();
        let config = worktree.gitdir.join("modules/sub/config");
        let stale = git_config(&config, &["core.worktree"]);
        let before = Snapshot::take(&old).unwrap();
        let new = scratch.path().join("moved/wt");
        relocate_worktree(&worktree, &new).expect("the move itself should work");

        git_config(&config, &["core.worktree", &stale]);
        let stop = verify(&before, &new).expect_err("a stale core.worktree went unnoticed");

        assert_eq!(stop.status(), 3, "{stop}");
        let report = stop.to_string();
        assert!(report.contains("sub"), "{report}");
        let kept: Vec<&str> = report
            .lines()
            .filter_map(|line| line.trim().strip_prefix("snapshot before: "))
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "the report does not say where the snapshot is: {report}"
        );
        assert!(fs::metadata(kept[0]).is_ok(), "{} is not there", kept[0]);
    }
}
