//! Finding a worktree's submodules and their clones, from what Git records.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::git::{Git, records, split_record, trimmed};
use crate::outcome::CannotTell;
use crate::paths::{self, cannot_read};

const GITLINK_MODE: &[u8] = b"160000";

/// A gitlink: a submodule's path and the commit recorded for it.
#[derive(Debug, PartialEq, Eq)]
pub struct Gitlink {
    pub path: PathBuf,
    pub commit: String,
}

/// Every git directory under `<gitdir>/modules`: populated submodules and
/// deinitialised leftovers alike, nested ones included. A git directory is
/// identified by its `objects` directory, which `logs/` and `refs/` never have,
/// although they hold files called `HEAD` too.
pub fn module_gitdirs(gitdir: &Path) -> Result<Vec<PathBuf>, CannotTell> {
    let modules = gitdir.join("modules");
    let mut found = Vec::new();
    if paths::is_dir(&modules)? {
        collect_gitdirs(&modules, &mut found)?;
    }
    found.sort();
    Ok(found)
}

fn collect_gitdirs(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), CannotTell> {
    let mut holds_head = false;
    for entry in fs::read_dir(dir).map_err(|error| cannot_read(dir, &error))? {
        let entry = entry.map_err(|error| cannot_read(dir, &error))?;
        let kind = entry
            .file_type()
            .map_err(|error| cannot_read(&entry.path(), &error))?;
        if kind.is_dir() {
            collect_gitdirs(&entry.path(), found)?;
        } else if kind.is_file() && entry.file_name() == "HEAD" {
            holds_head = true;
        }
    }
    if holds_head && paths::is_dir(&dir.join("objects"))? {
        found.push(dir.to_path_buf());
    }
    Ok(())
}

/// Commits a clone holds that no remote-tracking ref has: the one thing deleting a
/// submodule clone can lose. Empty means there are none. `--all` rather than
/// `--branches`, because a submodule normally sits on a detached HEAD, so a commit
/// made there is on no branch at all, and because a stash is work too.
pub fn unpushed(clone: &Path) -> Result<Vec<u8>, CannotTell> {
    Git::in_clone(clone, ["log", "--oneline", "--all", "--not", "--remotes"]).probe(clone)
}

/// The gitlinks at the top of `ls-files -s -z` output.
fn gitlinks_in_index(listing: &[u8]) -> Vec<Gitlink> {
    records(listing)
        .filter_map(split_record)
        .filter_map(|(fields, path)| {
            let mut fields = fields.split(|byte| *byte == b' ');
            let mode = fields.next()?;
            let commit = fields.next()?;
            gitlink(mode, commit, path)
        })
        .collect()
}

/// The gitlinks in `ls-tree -r -z` output.
fn gitlinks_in_tree(listing: &[u8]) -> Vec<Gitlink> {
    records(listing)
        .filter_map(split_record)
        .filter_map(|(fields, path)| {
            let mut fields = fields.split(|byte| *byte == b' ');
            let mode = fields.next()?;
            let commit = fields.nth(1)?;
            gitlink(mode, commit, path)
        })
        .collect()
}

fn gitlink(mode: &[u8], commit: &[u8], path: &OsStr) -> Option<Gitlink> {
    (mode == GITLINK_MODE).then(|| Gitlink {
        path: PathBuf::from(path),
        commit: String::from_utf8_lossy(commit).into_owned(),
    })
}

/// The gitlinks recorded in the index of the worktree or submodule at `dir`.
pub fn recorded_gitlinks(dir: &Path) -> Result<Vec<Gitlink>, CannotTell> {
    let listing = Git::at(dir, ["ls-files", "-s", "-z"]).probe(dir)?;
    Ok(gitlinks_in_index(&listing))
}

/// The commit `dir`'s index records for the gitlink at `path`.
pub fn recorded_commit(dir: &Path, path: &Path) -> Result<Option<String>, CannotTell> {
    let args: [&OsStr; 5] = [
        OsStr::new("ls-files"),
        OsStr::new("-s"),
        OsStr::new("-z"),
        OsStr::new("--"),
        path.as_os_str(),
    ];
    let listing = Git::at(dir, args).probe(dir)?;
    let links = gitlinks_in_index(&listing);
    Ok(links
        .into_iter()
        .find(|link| link.path == path)
        .map(|link| link.commit))
}

/// The gitlinks in `commit`'s tree, read from `clone`. `--full-tree`: with
/// `--work-tree=/`, git takes the caller's directory as a path prefix, and a bare
/// `ls-tree -r` lists nothing at all, which is how a modified nested submodule was
/// once emptied unchecked.
pub fn gitlinks_at(clone: &Path, commit: &str, label: &Path) -> Result<Vec<Gitlink>, CannotTell> {
    let listing =
        Git::in_clone(clone, ["ls-tree", "-r", "-z", "--full-tree", commit]).probe(label)?;
    Ok(gitlinks_in_tree(&listing))
}

/// Whether `dir` is a populated submodule: its `.git` exists.
pub fn is_populated(dir: &Path) -> Result<bool, CannotTell> {
    paths::exists(&dir.join(".git"))
}

/// A populated submodule whose `.git` file points at a git directory that no longer
/// exists: its clone is gone and git cannot run in it at all. An old guard made
/// exactly this shape, so it has to be recognised rather than just refused.
pub fn is_stranded(dir: &Path) -> Result<bool, CannotTell> {
    match paths::read_gitfile(dir)? {
        Some(pointer) => Ok(!paths::exists(pointer.as_path())?),
        None => Ok(false),
    }
}

/// Populated submodules under `top`, recursively, as paths relative to it. Found
/// from the gitlinks git records (mode 160000), never from `git submodule status`,
/// which reports a checked-out submodule as uninitialised when `.git/config` lacks
/// its url/active keys (measured 2026-09-22). A stranded submodule is listed but not
/// descended into, because git cannot read it.
pub fn populated(top: &Path) -> Result<Vec<PathBuf>, CannotTell> {
    let mut found = Vec::new();
    collect_populated(top, Path::new(""), &mut found)?;
    Ok(found)
}

fn collect_populated(
    top: &Path,
    prefix: &Path,
    found: &mut Vec<PathBuf>,
) -> Result<(), CannotTell> {
    for link in recorded_gitlinks(&top.join(prefix))? {
        let relative = prefix.join(&link.path);
        let dir = top.join(&relative);
        if !is_populated(&dir)? {
            continue;
        }
        found.push(relative.clone());
        if !is_stranded(&dir)? {
            collect_populated(top, &relative, found)?;
        }
    }
    Ok(())
}

/// Is `sub` inside another path of `all`? Top-level means "not inside another
/// submodule", NOT "no slash in its path": `lib/shared` is as top-level as `sub`.
#[must_use]
pub fn nested_in(sub: &Path, all: &[PathBuf]) -> bool {
    all.iter()
        .any(|other| other != sub && sub.starts_with(other))
}

/// A submodule's name at a commit, from that commit's `.gitmodules`. The name, not
/// the path, is what its clone lives under in `modules/`.
pub fn name_at(clone: &Path, commit: &str, path: &Path) -> Result<Option<OsString>, CannotTell> {
    let blob = format!("{commit}:.gitmodules");
    let listing = Git::in_clone(
        clone,
        [
            "config",
            "-z",
            "--blob",
            blob.as_str(),
            "--get-regexp",
            r"^submodule\..*\.path$",
        ],
    )
    .probe(clone)?;
    Ok(name_with_path(&listing, path))
}

/// `config -z` records are `<key>\n<value>`.
fn name_with_path(listing: &[u8], path: &Path) -> Option<OsString> {
    records(listing).find_map(|record| {
        let newline = record.iter().position(|byte| *byte == b'\n')?;
        let (key, value) = (&record[..newline], &record[newline + 1..]);
        if value != path.as_os_str().as_bytes() {
            return None;
        }
        let name = key.strip_prefix(b"submodule.")?.strip_suffix(b".path")?;
        Some(OsStr::from_bytes(name).to_owned())
    })
}

/// The first of the candidate clones that holds the commit, if any. Any clone of
/// the submodule will do: a commit is the same object in all of them.
pub fn clone_with(commit: &str, candidates: &[PathBuf]) -> Result<Option<PathBuf>, CannotTell> {
    let object = format!("{commit}^{{commit}}");
    for clone in candidates {
        if paths::is_dir(clone)?
            && Git::in_clone(clone, ["cat-file", "-e", object.as_str()]).succeeds(clone)?
        {
            return Ok(Some(clone.clone()));
        }
    }
    Ok(None)
}

/// The command that puts a clone of submodule `name` where the guard looks: its url
/// read from `gitdir`'s `.gitmodules` at `commit`, a relative one resolved against
/// `base`, the url of the repository that declares it.
pub fn clone_advice(
    gitdir: &Path,
    commit: &str,
    name: &OsStr,
    base: Option<&str>,
    target: &Path,
) -> String {
    let blob = format!("{commit}:.gitmodules");
    let mut key = OsString::from("submodule.");
    key.push(name);
    key.push(".url");
    let args: [&OsStr; 5] = [
        OsStr::new("config"),
        OsStr::new("--blob"),
        OsStr::new(&blob),
        OsStr::new("--get"),
        &key,
    ];
    let url = Git::in_clone(gitdir, args)
        .answer(gitdir)
        .ok()
        .flatten()
        .map_or_else(
            || "<its url>".to_owned(),
            |url| trimmed(url).to_string_lossy().into_owned(),
        );
    format!(
        "          git clone --bare {} {}",
        resolve_relative_url(&url, base),
        target.display()
    )
}

/// Resolves a relative submodule url as git resolves it: each `../` drops one
/// component of the base.
fn resolve_relative_url(url: &str, base: Option<&str>) -> String {
    let Some(base) = base.filter(|base| !base.is_empty()) else {
        return url.to_owned();
    };
    if !url.starts_with("./") && !url.starts_with("../") {
        return url.to_owned();
    }
    let mut base = base.strip_suffix('/').unwrap_or(base);
    let mut url = url;
    loop {
        if let Some(rest) = url.strip_prefix("./") {
            url = rest;
        } else if let Some(rest) = url.strip_prefix("../") {
            url = rest;
            base = base.rsplit_once('/').map_or(base, |(parent, _)| parent);
        } else {
            return format!("{base}/{url}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, git};

    const INDEX: &[u8] =
        b"100644 aaa 0\tREADME\x00160000 bbb 0\tlibs/my sub\x00100644 ccc 0\tname with\ttab\x00";

    #[test]
    fn index_gitlinks_keep_a_path_with_spaces_and_skip_files() {
        assert_eq!(
            gitlinks_in_index(INDEX),
            [Gitlink {
                path: PathBuf::from("libs/my sub"),
                commit: "bbb".into()
            }]
        );
    }

    #[test]
    fn tree_gitlinks_read_the_object_from_the_third_field() {
        let tree = b"100644 blob aaa\tREADME\x00160000 commit bbb\tsub dir\x00";
        assert_eq!(
            gitlinks_in_tree(tree),
            [Gitlink {
                path: PathBuf::from("sub dir"),
                commit: "bbb".into()
            }]
        );
    }

    #[test]
    fn a_path_with_a_slash_is_not_nested_by_that_alone() {
        let all = [PathBuf::from("lib/shared"), PathBuf::from("sub")];
        assert!(!nested_in(Path::new("lib/shared"), &all));
        assert!(!nested_in(Path::new("sub"), &all));
    }

    #[test]
    fn a_path_inside_another_is_nested_and_a_shared_prefix_is_not() {
        let all = [
            PathBuf::from("sub"),
            PathBuf::from("sub/nested"),
            PathBuf::from("subway"),
        ];
        assert!(nested_in(Path::new("sub/nested"), &all));
        assert!(!nested_in(Path::new("subway"), &all));
    }

    #[test]
    fn a_submodule_is_named_by_its_key_and_matched_by_its_path() {
        let listing = b"submodule.first.path\nlibs/a\x00submodule.my.name.path\nlibs/b c\x00";
        assert_eq!(
            name_with_path(listing, Path::new("libs/b c")),
            Some(OsString::from("my.name"))
        );
        assert_eq!(name_with_path(listing, Path::new("libs")), None);
    }

    #[test]
    fn a_relative_url_drops_one_base_component_per_parent() {
        let base = Some("https://host/group/repo.git");
        assert_eq!(
            resolve_relative_url("../sub.git", base),
            "https://host/group/sub.git"
        );
        assert_eq!(
            resolve_relative_url("./sub.git", base),
            "https://host/group/repo.git/sub.git"
        );
        assert_eq!(
            resolve_relative_url("../../x/sub.git", base),
            "https://host/x/sub.git"
        );
    }

    #[test]
    fn an_absolute_url_and_a_missing_base_are_left_alone() {
        assert_eq!(
            resolve_relative_url("https://h/s.git", Some("https://h/r")),
            "https://h/s.git"
        );
        assert_eq!(resolve_relative_url("../s.git", None), "../s.git");
        assert_eq!(resolve_relative_url("../s.git", Some("")), "../s.git");
    }

    #[test]
    fn module_gitdirs_finds_clones_at_any_depth_but_not_logs_or_refs() {
        let scratch = Scratch::new();
        let gitdir = scratch.path();
        for clone in ["modules/sub", "modules/sub/modules/nested"] {
            fs::create_dir_all(gitdir.join(clone).join("objects")).unwrap();
            fs::write(gitdir.join(clone).join("HEAD"), "x").unwrap();
        }
        for not_a_clone in ["modules/sub/logs", "modules/sub/refs/remotes/origin"] {
            fs::create_dir_all(gitdir.join(not_a_clone)).unwrap();
            fs::write(gitdir.join(not_a_clone).join("HEAD"), "x").unwrap();
        }
        assert_eq!(
            module_gitdirs(gitdir).unwrap(),
            [
                gitdir.join("modules/sub"),
                gitdir.join("modules/sub/modules/nested")
            ]
        );
    }

    #[test]
    fn no_modules_directory_means_no_clones() {
        let scratch = Scratch::new();
        assert_eq!(
            module_gitdirs(scratch.path()).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn a_stranded_submodule_is_one_whose_gitfile_leads_nowhere() {
        let scratch = Scratch::new();
        let dir = scratch.path();
        assert!(!is_stranded(dir).unwrap(), "no .git at all");
        fs::write(dir.join(".git"), "gitdir: missing/place\n").unwrap();
        assert!(is_stranded(dir).unwrap());
        fs::create_dir_all(dir.join("missing/place")).unwrap();
        assert!(!is_stranded(dir).unwrap());
    }

    #[test]
    fn a_detached_commit_and_a_stash_both_count_as_unpushed() {
        let scratch = Scratch::new();
        let remote = scratch.path().join("remote.git");
        let clone = scratch.path().join("clone");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare"]);
        git(
            scratch.path(),
            &["clone", "-q", remote.to_str().unwrap(), "clone"],
        );
        fs::write(clone.join("f"), "1").unwrap();
        git(&clone, &["add", "f"]);
        git(&clone, &["commit", "-qm", "pushed"]);
        git(&clone, &["push", "-q", "origin", "HEAD"]);
        let gitdir = clone.join(".git");
        let holds_unpushed = || !unpushed(&gitdir).unwrap().is_empty();
        assert!(!holds_unpushed(), "everything is pushed");

        fs::write(clone.join("f"), "2").unwrap();
        git(&clone, &["stash", "-q"]);
        assert!(holds_unpushed(), "a stash is work too");
        git(&clone, &["stash", "drop", "-q"]);
        assert!(!holds_unpushed());

        git(&clone, &["checkout", "-q", "--detach"]);
        fs::write(clone.join("f"), "3").unwrap();
        git(&clone, &["commit", "-qam", "detached"]);
        assert!(holds_unpushed(), "on no branch at all");
    }

    #[test]
    fn a_clone_that_cannot_be_read_is_could_not_tell_not_nothing_unpushed() {
        let scratch = Scratch::new();
        assert!(unpushed(&scratch.path().join("nothing-here")).is_err());
    }
}
