//! Copies the gitignored files a repository chose into a new worktree, so that it works
//! without manual setup. A repository opts in with a committed `.worktreeinclude`, in
//! gitignore syntax.
//!
//! This hands local files, often secrets, to a checkout of a branch, so the checks are
//! about who decides. The list is read from the main checkout's HEAD, never the worktree:
//! as with direnv trust, a branch must not be able to widen what it receives. A file is
//! copied only if the list selects it and git ignores it, so the list cannot be used to
//! shadow a tracked file. It never overwrites, and it never writes through a symlink a
//! branch committed. Provisioning the main checkout itself copies nothing, because every
//! file it would write already exists.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use crate::commands::provision::main_checkout;
use crate::git::{Git, flag, records};
use crate::outcome::{CannotTell, Outcome};
use crate::say::{Sink, say};
use crate::temp::TempFile;

const LIST: &str = "HEAD:.worktreeinclude";

pub fn run(path: &Path, sink: Sink) -> Outcome {
    let Some(main) = main_checkout(path)? else {
        return Ok(());
    };
    let chosen = chosen_files(&main)?;
    let copied = chosen
        .iter()
        .filter(|file| copy_one(&main, path, file, sink))
        .count();
    if copied > 0 {
        say(
            sink,
            format_args!("copied {copied} seed file(s) from {}", main.display()),
        );
    }
    Ok(())
}

/// The files the list selects, among those git ignores in the main checkout.
fn chosen_files(main: &Path) -> Outcome<Vec<PathBuf>> {
    let has_list = Git::at(main, ["cat-file", "-e", LIST]).succeeds(main)?;
    if !has_list {
        return Ok(Vec::new());
    }
    let list = Git::at(main, ["show", LIST]).probe(main)?;
    let patterns = TempFile::create()
        .and_then(|file| fs::write(file.path(), &list).map(|()| file))
        .map_err(|error| {
            CannotTell::new(format!("worktree: cannot write a temporary file: {error}"))
        })?;
    let selected = Git::at(
        main,
        [
            "ls-files".into(),
            "-z".into(),
            "--others".into(),
            "--ignored".into(),
            flag("--exclude-from=", patterns.path()),
        ],
    )
    .probe(main)?;
    let ignored = Git::at(
        main,
        [
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
        ],
    )
    .probe(main)?;
    let ignored: Vec<&[u8]> = records(&ignored).collect();
    Ok(records(&selected)
        .filter(|file| ignored.contains(file))
        .map(|file| PathBuf::from(std::ffi::OsStr::from_bytes(file)))
        .collect())
}

/// Reports whether it copied. A failure warns and costs only this one file.
fn copy_one(main: &Path, worktree: &Path, file: &Path, sink: Sink) -> bool {
    match copy_file(main, worktree, file) {
        Ok(copied) => copied,
        Err(error) => {
            say(
                sink,
                format_args!("seed: not copying {}: {error}", file.display()),
            );
            false
        }
    }
}

fn copy_file(main: &Path, worktree: &Path, file: &Path) -> io::Result<bool> {
    let from = main.join(file);
    let source = fs::symlink_metadata(&from)?;
    if !source.is_file() {
        return Ok(false);
    }
    make_parents_inside(worktree, file)?;
    let to = worktree.join(file);
    let mut target = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(source.mode() & 0o777)
        .open(&to)
    {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    let copied = File::open(&from).and_then(|mut reader| io::copy(&mut reader, &mut target));
    if let Err(error) = copied {
        drop(fs::remove_file(&to));
        return Err(error);
    }
    Ok(true)
}

/// Creates the file's parent directories, refusing to pass through a symlink.
/// `create_new` protects only the last component.
fn make_parents_inside(worktree: &Path, file: &Path) -> io::Result<()> {
    let mut at = worktree.to_path_buf();
    let Some(parent) = file.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::other("the path is not plain"));
        };
        at.push(name);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} is not a directory, so the file would land outside the worktree",
                    at.display()
                )));
            }
            // Anything else that stopped the look stops `create_dir` too, which never
            // follows a symlink, so it needs no case of its own.
            Err(_) => fs::create_dir(&at)?,
        }
    }
    Ok(())
}
