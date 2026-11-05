//! Leftovers of removed worktrees, found, and deleted only with `--apply`. No timer
//! runs this: an unattended delete is a new way to lose data.
//!
//! Under each placement root, a directory at depth 1 or 2 is one of:
//!
//! - **live**: a registered worktree, whose `.git` file points at a git directory that
//!   points back. Untouched, and so is everything above it.
//! - **orphan**: has a `.git` file whose git directory is gone. Reported, never
//!   deleted: its files may be uncommitted work nothing else holds.
//! - **repo**: a full clone, with a `.git` directory. Untouched.
//! - **other**: anything else, such as ignored files a removal left behind, or an
//!   empty shell. A leftover, deleted by `--apply`.
//!
//! And per repository (`--repo`, or every repository a live worktree under a root
//! belongs to, or the one you are in): registrations whose worktree directory is gone.
//! `git worktree prune` would delete their git directories, and with them any
//! submodule clone inside, so their clones are checked for unpushed commits first.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::git::{Git, trimmed_path};
use crate::outcome::{CannotTell, Outcome, Stop};
use crate::paths::{self, ResolvedPath, cannot_read};
use crate::say::{Sink, say};
use crate::submodules;
use crate::worktree::parent_of;

/// `du`'s block size for `st_blocks`.
const BLOCK_BYTES: u64 = 512;

/// Whether to delete what was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    DryRun,
    Apply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Live,
    Orphan,
    Repo,
    Other,
}

/// What a placement root holds.
#[derive(Debug, Default)]
pub struct Found {
    pub live: Vec<PathBuf>,
    pub orphans: Vec<PathBuf>,
    leftovers: Vec<PathBuf>,
    repos: Vec<PathBuf>,
}

/// Classifies what is under one canonical placement root, changing nothing.
pub fn survey(root: &Path) -> Result<Found, CannotTell> {
    let mut found = Found::default();
    survey_root(root, &mut found)?;
    Ok(found)
}

pub fn run(mode: Mode, repos: Vec<PathBuf>, roots: Vec<PathBuf>) -> Outcome {
    let roots = if roots.is_empty() {
        vec![default_root()?]
    } else {
        roots
    };

    let mut found = Found {
        repos,
        ..Found::default()
    };
    // The repository the caller is in, if any. Being outside one is a normal answer.
    let here = Git::new(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .answer(Path::new("."))?;
    if let Some(common) = here {
        found.repos.push(parent_of(&trimmed_path(common))?);
    }
    for root in &roots {
        if paths::is_dir(root)? {
            let root = root
                .canonicalize()
                .map_err(|error| cannot_read(root, &error))?;
            survey_root(&root, &mut found)?;
        }
    }

    for orphan in &found.orphans {
        say(
            Sink::Stdout,
            format_args!(
                "orphan   {} — a .git file whose gitdir is gone; inspect by hand, never swept",
                orphan.display()
            ),
        );
    }
    for leftover in &found.leftovers {
        match mode {
            Mode::Apply => {
                fs::remove_dir_all(leftover).map_err(|error| {
                    Stop::refused(format!("cannot delete {}: {error}", leftover.display()))
                })?;
                say(
                    Sink::Stdout,
                    format_args!("deleted  {}", leftover.display()),
                );
            }
            Mode::DryRun => say(
                Sink::Stdout,
                format_args!(
                    "leftover {} ({})",
                    leftover.display(),
                    human_size(disk_usage(leftover))
                ),
            ),
        }
    }

    let mut seen = Vec::new();
    for repo in &found.repos {
        if !paths::is_dir(repo)? {
            continue;
        }
        let repo = repo
            .canonicalize()
            .map_err(|error| cannot_read(repo, &error))?;
        if !seen.contains(&repo) {
            sweep_registrations(&repo, mode)?;
            seen.push(repo);
        }
    }

    if mode == Mode::DryRun {
        say(
            Sink::Stdout,
            "dry run — nothing deleted; pass --apply to delete the leftovers listed",
        );
    }
    Ok(())
}

fn default_root() -> Outcome<PathBuf> {
    env::var_os("WORKTREE_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Stop::refused("sweep: no placement root given and WORKTREE_ROOT is unset"))
}

/// Depth 1 is a group (`<root>/<repo>`) or, in a flat layout, a worktree. A group
/// holding nothing kept is one leftover, reported once; otherwise each of its
/// non-worktree children is.
fn survey_root(root: &Path, found: &mut Found) -> Result<(), CannotTell> {
    for group in paths::subdirectories(root)? {
        let kind = classify(&group)?;
        if kind != Kind::Other {
            note(kind, group, found)?;
            continue;
        }
        let mut kept = false;
        let mut others = Vec::new();
        for child in paths::subdirectories(&group)? {
            let kind = classify(&child)?;
            if kind == Kind::Other {
                others.push(child);
            } else {
                kept = true;
                note(kind, child, found)?;
            }
        }
        if kept {
            found.leftovers.append(&mut others);
        } else {
            found.leftovers.push(group);
        }
    }
    Ok(())
}

/// A live worktree names its repository for the registration pass.
fn note(kind: Kind, dir: PathBuf, found: &mut Found) -> Result<(), CannotTell> {
    match kind {
        Kind::Live => {
            let common = Git::at(
                &dir,
                ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .probe(&dir)?;
            found.repos.push(parent_of(&trimmed_path(common))?);
            found.live.push(dir);
        }
        Kind::Orphan => found.orphans.push(dir),
        Kind::Repo | Kind::Other => {}
    }
    Ok(())
}

fn classify(dir: &Path) -> Result<Kind, CannotTell> {
    let dot_git = dir.join(".git");
    if paths::is_dir(&dot_git)? {
        return Ok(Kind::Repo);
    }
    if !paths::is_file(&dot_git)? {
        return Ok(Kind::Other);
    }
    let Some(admin) = paths::read_gitfile(dir)? else {
        return Ok(Kind::Orphan);
    };
    if !paths::is_file(&admin.as_path().join("gitdir"))? {
        return Ok(Kind::Orphan);
    }
    let here = ResolvedPath::resolve(dir, OsStr::new(".git"))?;
    if here == paths::read_backpointer(admin.as_path())? {
        Ok(Kind::Live)
    } else {
        Ok(Kind::Orphan)
    }
}

/// Registrations whose directory is gone, each judged on its own: `git worktree prune`
/// works on the whole repository, so one registration holding an unpushed clone would
/// otherwise keep every clean one alive. A clean one is pruned by deleting its admin
/// directory, exactly what prune does per entry. Locked registrations are left alone,
/// as prune leaves them.
fn sweep_registrations(repo: &Path, mode: Mode) -> Outcome {
    let common = trimmed_path(
        Git::at(
            repo,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .probe(repo)?,
    );
    for admin in registrations(&common)? {
        if !paths::is_file(&admin.join("gitdir"))? || paths::exists(&admin.join("locked"))? {
            continue;
        }
        let back = paths::read_backpointer(&admin)?;
        let worktree = parent_of(back.as_path())?;
        if paths::exists(&worktree)? {
            continue;
        }
        let mut blocked = false;
        for clone in submodules::module_gitdirs(&admin)? {
            if !submodules::unpushed(&clone)?.is_empty() {
                say(
                    Sink::Stdout,
                    format_args!(
                        "stale    {} — its submodule clone {} holds commits no remote has; NOT pruning it",
                        worktree.display(),
                        clone.display()
                    ),
                );
                blocked = true;
            }
        }
        if blocked {
            continue;
        }
        match mode {
            Mode::Apply => {
                fs::remove_dir_all(&admin).map_err(|error| {
                    Stop::refused(format!("cannot delete {}: {error}", admin.display()))
                })?;
                say(
                    Sink::Stdout,
                    format_args!(
                        "pruned   {} (registration in {})",
                        worktree.display(),
                        repo.display()
                    ),
                );
            }
            Mode::DryRun => say(
                Sink::Stdout,
                format_args!(
                    "stale    {} (registered in {}; its directory is gone)",
                    worktree.display(),
                    repo.display()
                ),
            ),
        }
    }
    Ok(())
}

/// `<common>/worktrees/*`: a glob's view, so dotfiles do not count.
fn registrations(common: &Path) -> Result<Vec<PathBuf>, CannotTell> {
    let worktrees = common.join("worktrees");
    if !paths::is_dir(&worktrees)? {
        return Ok(Vec::new());
    }
    let mut admins = paths::subdirectories(&worktrees)?;
    admins.retain(|admin| {
        admin
            .file_name()
            .is_some_and(|name| !name.as_bytes().starts_with(b"."))
    });
    Ok(admins)
}

/// Bytes on disk, as `du` counts them. Only a label on a dry run's report, so what
/// cannot be read counts as nothing, as it did for `du -s 2>/dev/null`.
fn disk_usage(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = metadata.blocks() * BLOCK_BYTES;
    if metadata.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            total += disk_usage(&entry.path());
        }
    }
    total
}

/// `du -h`'s spelling: one decimal below 10, rounded up.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes == 0 {
        return "0".to_owned();
    }
    let mut unit_bytes = 1024_u64;
    let mut unit = 0;
    while bytes >= unit_bytes.saturating_mul(1024) && unit + 1 < UNITS.len() {
        unit_bytes *= 1024;
        unit += 1;
    }
    let tenths = (u128::from(bytes) * 10).div_ceil(u128::from(unit_bytes));
    if tenths < 100 {
        format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit])
    } else {
        format!("{}{}", bytes.div_ceil(unit_bytes), UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    #[test]
    fn sizes_are_spelled_as_du_spells_them() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(4096), "4.0K");
        assert_eq!(human_size(12 * 1024), "12K");
        assert_eq!(human_size(1024 * 1024 + 1), "1.1M");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(10 * 1024), "10K");
        assert_eq!(human_size(9 * 1024 + 900), "9.9K");
        assert_eq!(human_size(1 << 60), "1.0E");
        assert_eq!(human_size(u64::MAX), "16E");
    }

    #[test]
    fn a_directory_with_no_git_entry_is_other() {
        let scratch = Scratch::new();
        assert_eq!(classify(scratch.path()).unwrap(), Kind::Other);
    }

    #[test]
    fn a_git_directory_is_a_repo() {
        let scratch = Scratch::new();
        fs::create_dir(scratch.path().join(".git")).unwrap();
        assert_eq!(classify(scratch.path()).unwrap(), Kind::Repo);
    }

    #[test]
    fn a_gitfile_leading_nowhere_is_an_orphan() {
        let scratch = Scratch::new();
        fs::write(scratch.path().join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
        assert_eq!(classify(scratch.path()).unwrap(), Kind::Orphan);
    }

    #[test]
    fn a_gitfile_with_no_pointer_is_an_orphan() {
        let scratch = Scratch::new();
        fs::write(scratch.path().join(".git"), "junk\n").unwrap();
        assert_eq!(classify(scratch.path()).unwrap(), Kind::Orphan);
    }

    #[test]
    fn relative_pointers_each_resolve_from_their_holder_to_make_a_live_worktree() {
        let scratch = Scratch::new();
        let wt = scratch.path().join("elsewhere/wt");
        let admin = scratch.path().join("repo/.git/worktrees/wt");
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(&admin).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../../repo/.git/worktrees/wt\n").unwrap();
        fs::write(admin.join("gitdir"), "../../../../elsewhere/wt/.git\n").unwrap();
        assert_eq!(classify(&wt).unwrap(), Kind::Live);
    }

    #[test]
    fn a_registration_pointing_at_another_directory_is_not_live() {
        let scratch = Scratch::new();
        let wt = scratch.path().join("wt");
        let admin = scratch.path().join("admin");
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(&admin).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../admin\n").unwrap();
        fs::write(admin.join("gitdir"), "/some/other/wt/.git\n").unwrap();
        assert_eq!(classify(&wt).unwrap(), Kind::Orphan);
    }

    #[test]
    fn disk_usage_counts_what_is_inside_a_directory() {
        let scratch = Scratch::new();
        let file = scratch.path().join("f");
        fs::write(&file, vec![1_u8; 65536]).unwrap();
        let blocks = |path: &Path| fs::symlink_metadata(path).unwrap().blocks();
        assert_eq!(
            disk_usage(scratch.path()),
            (blocks(scratch.path()) + blocks(&file)) * BLOCK_BYTES
        );
    }
}
