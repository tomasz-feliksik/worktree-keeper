//! What the unit tests share: a scratch directory and a real `git`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A directory removed when the test ends. Canonical, so that a test can compare
/// what the tool prints with paths built from it.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> Self {
        let unique = format!(
            "worktree-keeper-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Runs git in `dir` with no personal configuration, and insists it succeeded.
pub fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

const FILE_TRANSPORT: [&str; 2] = ["-c", "protocol.file.allow=always"];

/// A worktree on branch `w` of a project whose submodule `sub` has a nested `nested`,
/// all initialised, with `sub` on a local branch of its own.
pub fn worktree_with_nested_submodules(root: &Path) -> PathBuf {
    for name in ["nested", "sub"] {
        git(root, &["init", "-q", "--bare", &format!("{name}.git")]);
        git(root, &["clone", "-q", &format!("{name}.git"), name]);
        fs::write(root.join(name).join("README"), name).unwrap();
        git(&root.join(name), &["add", "README"]);
        git(&root.join(name), &["commit", "-q", "-m", name]);
        git(&root.join(name), &["push", "-q", "origin", "HEAD"]);
    }
    let nested_url = root.join("nested.git");
    let mut add = FILE_TRANSPORT.to_vec();
    add.extend([
        "submodule",
        "-q",
        "add",
        nested_url.to_str().unwrap(),
        "nested",
    ]);
    git(&root.join("sub"), &add);
    git(&root.join("sub"), &["commit", "-q", "-m", "nested"]);
    git(&root.join("sub"), &["push", "-q", "origin", "HEAD"]);

    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    let sub_url = root.join("sub.git");
    let mut add = FILE_TRANSPORT.to_vec();
    add.extend(["submodule", "-q", "add", sub_url.to_str().unwrap(), "sub"]);
    git(&project, &add);
    git(&project, &["commit", "-q", "-m", "sub"]);
    let wt = root.join("wt");
    git(
        &project,
        &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
    );
    let mut init = FILE_TRANSPORT.to_vec();
    init.extend(["submodule", "-q", "update", "--init", "--recursive"]);
    git(&wt, &init);
    git(&wt.join("sub"), &["checkout", "-q", "-b", "local"]);
    wt
}
