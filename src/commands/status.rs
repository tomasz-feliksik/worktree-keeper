//! `status`: every worktree under the placement root, and whether it could be removed.
//! Changes nothing. Anything that could not be read is `null` in the JSON and makes the
//! verdict `unknown`, never `removable`. See docs/json-schema.md.

use std::env;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::commands::check::{Format, Reason, SCHEMA_VERSION, Verdict};
use crate::commands::guard::{self, OwnChanges, Rules, RunningProcesses};
use crate::commands::remove::{Merged, merged_state, refuse_unmerged_branch};
use crate::commands::sweep;
use crate::git::{Git, shown};
use crate::in_use::{self, Holder};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::paths::{self, cannot_read};
use crate::say;
use crate::submodules;
use crate::worktree::LinkedWorktree;

/// The rules `worktree remove` applies with no options.
const PLAIN_REMOVE: Rules = Rules {
    own_changes: OwnChanges::MustBeClean,
    running: RunningProcesses::Refuse,
};

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    worktrees: Vec<Row>,
}

#[derive(Debug, Serialize)]
struct Row {
    path: String,
    repo: Option<String>,
    branch: Option<String>,
    dirty: Option<bool>,
    unpushed: Option<bool>,
    merged: Option<bool>,
    locked: Option<String>,
    in_use: Option<Vec<ProcessRow>>,
    verdict: Verdict,
    reasons: Vec<Reason>,
}

#[derive(Debug, Serialize)]
struct ProcessRow {
    pid: u32,
    name: String,
}

impl From<Holder> for ProcessRow {
    fn from(holder: Holder) -> Self {
        Self {
            pid: holder.pid,
            name: holder.name,
        }
    }
}

pub fn run(path: Option<PathBuf>, format: Format) -> Outcome {
    let rows = rows_for(path)?;
    let unknown = rows
        .iter()
        .filter(|row| row.verdict == Verdict::Unknown)
        .count();
    match format {
        Format::Json => say::print_json(&Report {
            schema_version: SCHEMA_VERSION,
            worktrees: rows,
        })?,
        Format::Text => print_text(&rows),
    }
    if unknown > 0 {
        return Err(CannotTell::new(format!(
            "worktree: could not read the state of {unknown} worktree(s), so they are unknown rather than safe"
        ))
        .into());
    }
    Ok(())
}

/// A path that is a worktree shows only it; any other directory is a placement root.
fn rows_for(path: Option<PathBuf>) -> Outcome<Vec<Row>> {
    let root = match path {
        Some(path) => {
            if paths::exists(&path.join(".git"))? {
                return Ok(vec![row_for_live(&path)]);
            }
            path
        }
        None => env::var_os("WORKTREE_ROOT")
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                Stop::refused("status: no placement root given and WORKTREE_ROOT is unset")
            })?,
    };
    if !paths::is_dir(&root)? {
        return Ok(Vec::new());
    }
    let root = root
        .canonicalize()
        .map_err(|error| cannot_read(&root, &error))?;
    let found = sweep::survey(&root)?;
    let mut rows: Vec<Row> = found.live.iter().map(|path| row_for_live(path)).collect();
    rows.extend(found.orphans.iter().map(|path| row_for_orphan(path)));
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(rows)
}

fn row_for_orphan(path: &Path) -> Row {
    let reason = Reason {
        kind: "orphan".to_owned(),
        message: format!(
            "{} has a .git file whose gitdir is gone; inspect it by hand",
            path.display()
        ),
    };
    unreadable_row(path, reason)
}

fn unreadable_row(path: &Path, reason: Reason) -> Row {
    Row {
        path: path.display().to_string(),
        repo: None,
        branch: None,
        dirty: None,
        unpushed: None,
        merged: None,
        locked: None,
        in_use: None,
        verdict: Verdict::Unknown,
        reasons: vec![reason],
    }
}

fn row_for_live(path: &Path) -> Row {
    let worktree = match LinkedWorktree::resolve(path) {
        Ok(worktree) => worktree,
        Err(stop) => {
            let mut row = unreadable_row(path, Reason::of(&stop));
            row.verdict = Verdict::of(&stop);
            return row;
        }
    };
    let wt = &worktree.path;
    let branch = Git::at(wt, ["branch", "--show-current"])
        .probe(wt)
        .map(|name| shown(&name));
    let merged = branch
        .as_ref()
        .ok()
        .map(|name| merged_state(&worktree, name));
    let facets = Facets {
        dirty: guard::status(wt).map(|dirty| !dirty.is_empty()).ok(),
        unpushed: holds_unpushed(&worktree).ok(),
        in_use: in_use::holders(wt).ok(),
        locked: guard::lock_reason(&worktree),
        merged: merged.and_then(Result::ok),
        branch_known: branch.is_ok(),
    };
    let answer = verdict_of(&worktree);
    let (verdict, reasons) = settle(answer, &facets);
    let Facets {
        dirty,
        unpushed,
        in_use,
        locked,
        merged,
        ..
    } = facets;
    Row {
        path: wt.display().to_string(),
        repo: Some(worktree.main_checkout.display().to_string()),
        branch: branch.ok().filter(|name| !name.is_empty()),
        dirty,
        unpushed,
        merged: match merged {
            Some(Merged::Yes | Merged::Integrated) => Some(true),
            Some(Merged::No) => Some(false),
            Some(Merged::NoBranch) | None => None,
        },
        locked: locked.ok().flatten(),
        in_use: in_use.map(|holders| holders.into_iter().map(ProcessRow::from).collect()),
        verdict,
        reasons,
    }
}

struct Facets {
    dirty: Option<bool>,
    unpushed: Option<bool>,
    in_use: Option<Vec<Holder>>,
    locked: Result<Option<String>, CannotTell>,
    merged: Option<Merged>,
    branch_known: bool,
}

impl Facets {
    /// Whether anything shown could not be read.
    fn any_unread(&self) -> bool {
        self.dirty.is_none()
            || self.unpushed.is_none()
            || self.in_use.is_none()
            || self.locked.is_err()
            || self.merged.is_none()
            || !self.branch_known
    }
}

/// What `worktree remove` with no options would say, short of acting.
fn verdict_of(worktree: &LinkedWorktree) -> Outcome {
    guard::assess(worktree, PLAIN_REMOVE).map(drop)?;
    refuse_unmerged_branch(worktree).map(drop)
}

/// A verdict can only be `removable` if everything shown beside it was read.
fn settle(answer: Outcome, facets: &Facets) -> (Verdict, Vec<Reason>) {
    match answer {
        Err(stop) => (Verdict::of(&stop), vec![Reason::of(&stop)]),
        Ok(()) if facets.any_unread() => (
            Verdict::Unknown,
            vec![Reason {
                kind: Stop::from(CannotTell::new(String::new()))
                    .kind_name()
                    .to_owned(),
                message: "part of this worktree's state could not be read".to_owned(),
            }],
        ),
        Ok(()) => (Verdict::Removable, Vec::new()),
    }
}

fn holds_unpushed(worktree: &LinkedWorktree) -> Result<bool, CannotTell> {
    for clone in submodules::module_gitdirs(&worktree.gitdir)? {
        if !submodules::unpushed(&clone)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn print_text(rows: &[Row]) {
    for row in rows {
        let branch = row.branch.as_deref().unwrap_or("(no branch)");
        let verdict = match row.verdict {
            Verdict::Removable => "removable",
            Verdict::Refused => "refused",
            Verdict::Unknown => "unknown",
        };
        let why = row
            .reasons
            .first()
            .map_or(String::new(), |reason| format!(" — {}", reason.kind));
        say::print_text_line(&format!("{verdict:<9} {branch:<30} {}{why}", row.path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_read() -> Facets {
        Facets {
            dirty: Some(false),
            unpushed: Some(false),
            in_use: Some(Vec::new()),
            locked: Ok(None),
            merged: Some(Merged::Yes),
            branch_known: true,
        }
    }

    #[test]
    fn a_clean_answer_with_everything_read_is_removable() {
        assert_eq!(settle(Ok(()), &all_read()).0, Verdict::Removable);
    }

    #[test]
    fn a_clean_answer_with_anything_unread_is_unknown_never_removable() {
        let cases: [fn(&mut Facets); 6] = [
            |facets| facets.dirty = None,
            |facets| facets.unpushed = None,
            |facets| facets.in_use = None,
            |facets| facets.locked = Err(CannotTell::new("x")),
            |facets| facets.merged = None,
            |facets| facets.branch_known = false,
        ];
        for break_one in cases {
            let mut facets = all_read();
            break_one(&mut facets);
            let (verdict, reasons) = settle(Ok(()), &facets);
            assert_eq!(verdict, Verdict::Unknown);
            assert_eq!(reasons[0].kind, "could_not_check");
        }
    }

    #[test]
    fn a_refusal_and_could_not_check_keep_their_own_verdicts() {
        let refused = settle(Err(Stop::refused("no")), &all_read());
        assert_eq!(refused.0, Verdict::Refused);
        let unknown = settle(Err(CannotTell::new("?").into()), &all_read());
        assert_eq!(unknown.0, Verdict::Unknown);
    }
}
