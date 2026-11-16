//! What git cannot vouch for after a move: the files it ignores. A virtualenv's scripts, a
//! build directory's dependency files, a symlink — anything a tool wrote there may hold the
//! worktree's absolute path, and keeps pointing at the old place once the worktree moves.
//!
//! This finds such entries without knowing what made them: it asks git which entries it
//! ignores and searches each for the old path's bytes, stopping at the first file that holds
//! it. Remaking them is left to whoever knows what they are.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::git::Git;
use crate::outcome::CannotTell;
use crate::submodules;

/// An entry git ignores that still names the old path, and one file in it that does. Both
/// are relative to the worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Stale {
    pub entry: String,
    pub file: String,
}

/// The ignored entries of `worktree`, and of each populated submodule in it, that still
/// name `old`. Every entry is searched: one that cannot be read is "cannot tell", never
/// "holds nothing".
pub fn entries_naming(worktree: &Path, old: &Path) -> Result<Vec<Stale>, CannotTell> {
    let needle = old.as_os_str().as_bytes();
    if needle.is_empty() {
        // Every file would "contain" an empty path.
        return Err(CannotTell::new("worktree: no old path to search for"));
    }
    let mut tops = vec![PathBuf::new()];
    tops.extend(submodules::populated(worktree)?);
    let mut found = Vec::new();
    for top in tops {
        let dir = worktree.join(&top);
        let listed = Git::at(
            &dir,
            [
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
            ],
        )
        .probe(&dir)?;
        for entry in listed
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry = top.join(std::ffi::OsStr::from_bytes(entry));
            let held = first_holding(&worktree.join(&entry), needle)
                .map_err(|(path, error)| unreadable(&path, &error))?;
            if let Some(file) = held {
                let file = file.strip_prefix(worktree).unwrap_or(&file);
                found.push(Stale {
                    entry: entry.display().to_string(),
                    file: file.display().to_string(),
                });
            }
        }
    }
    Ok(found)
}

fn unreadable(path: &Path, error: &io::Error) -> CannotTell {
    CannotTell::new(format!(
        "worktree: cannot read {}, so cannot tell whether it still names the old path: {error}",
        path.display()
    ))
}

/// The first file under `path`, in name order, that holds `needle`: a symlink whose target
/// does, or a regular file whose bytes do. Symlinks are never followed.
fn first_holding(path: &Path, needle: &[u8]) -> Result<Option<PathBuf>, (PathBuf, io::Error)> {
    let fail = |error| (path.to_path_buf(), error);
    let meta = fs::symlink_metadata(path).map_err(fail)?;
    if meta.is_symlink() {
        let target = fs::read_link(path).map_err(fail)?;
        return Ok(names(target.as_os_str().as_bytes(), needle).then(|| path.to_path_buf()));
    }
    if meta.is_dir() {
        let mut children: Vec<PathBuf> = fs::read_dir(path)
            .map_err(fail)?
            .map(|child| child.map(|child| child.path()))
            .collect::<Result<_, _>>()
            .map_err(fail)?;
        children.sort();
        for child in children {
            if let Some(found) = first_holding(&child, needle)? {
                return Ok(Some(found));
            }
        }
        return Ok(None);
    }
    if meta.is_file() {
        let file = File::open(path).map_err(fail)?;
        return Ok(stream_names(file, needle)
            .map_err(fail)?
            .then(|| path.to_path_buf()));
    }
    // A socket or a pipe holds no path.
    Ok(None)
}

/// Whether `reader` names `needle`, read in pieces so a large file is never held whole. A
/// match whose next byte has not arrived yet stays in the kept tail and is decided later.
fn stream_names(mut reader: impl Read, needle: &[u8]) -> io::Result<bool> {
    const PIECE: usize = 64 * 1024;
    let mut window = Vec::with_capacity(PIECE + needle.len());
    let mut piece = vec![0; PIECE];
    loop {
        let read = reader.read(&mut piece)?;
        if read == 0 {
            return Ok(names(&window, needle));
        }
        window.extend_from_slice(&piece[..read]);
        let settled = matches(&window, needle)
            .any(|end| window.get(end).is_some_and(|next| !continues_a_name(*next)));
        if settled {
            return Ok(true);
        }
        let keep = window.len().min(needle.len());
        window.drain(..window.len() - keep);
    }
}

/// Whether `haystack` names the path `needle`: holds it, not followed by a character that
/// would make it a longer name (`/t/a` does not name `/t/ab`).
fn names(haystack: &[u8], needle: &[u8]) -> bool {
    matches(haystack, needle).any(|end| {
        haystack
            .get(end)
            .is_none_or(|next| !continues_a_name(*next))
    })
}

/// Where each occurrence of `needle` in `haystack` ends.
fn matches<'a>(haystack: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(move |(_, window)| *window == needle)
        .map(move |(start, _)| start + needle.len())
}

fn continues_a_name(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_named_only_where_its_name_ends() {
        assert!(names(b"#!/t/a/.venv/bin/python", b"/t/a"));
        assert!(names(b"cd /t/a", b"/t/a"));
        assert!(names(b"\"/t/a\"", b"/t/a"));
        assert!(!names(b"/t/ab/x", b"/t/a"));
        assert!(!names(b"/t/a.old", b"/t/a"));
        assert!(!names(b"/t/", b"/t/a"));
    }

    #[test]
    fn a_name_split_across_pieces_is_still_found() {
        let mut text = vec![b'x'; 64 * 1024 - 2];
        text.extend_from_slice(b"/t/a/bin\n");
        assert!(stream_names(text.as_slice(), b"/t/a").unwrap());
        let mut longer = vec![b'x'; 64 * 1024 - 4];
        longer.extend_from_slice(b"/t/ab");
        assert!(!stream_names(longer.as_slice(), b"/t/a").unwrap());
    }
}
