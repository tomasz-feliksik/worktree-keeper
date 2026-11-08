//! The default branch is the repository's, not the worktree's. A worktree can have it
//! checked out, and removing that worktree must never take the branch with it: merged and
//! pushed, `git branch -d` agrees to delete it, and nothing would say it had gone.

use crate::support::{Sandbox, Worktree, exists};

/// A worktree with the default branch checked out: the main checkout lets go of it first,
/// because git checks a branch out in one place only.
fn worktree_on_main(s: &Sandbox, name: &str) -> Worktree {
    s.git(&s.main, &["checkout", "-q", "--detach"]);
    let path = format!("{}/wt-{name}", s.t);
    s.git(&s.main, &["worktree", "add", "-q", &path, "main"]);
    s.git(
        &path,
        &["submodule", "-q", "update", "--init", "--recursive"],
    );
    let gitdir = s.git(&path, &["rev-parse", "--absolute-git-dir"]);
    Worktree { path, gitdir }
}

// Merged and pushed: the worktree goes and the branch stays, locally and on the remote,
// whatever the flags ask for the branch.
#[test]
fn remove_keeps_the_default_branch() {
    let s = Sandbox::with_project();
    let bare = format!("{}/project.git", s.t);

    let plain = worktree_on_main(&s, "plain");
    let removed = s.run(&["remove", &plain.path]);
    removed.rc(0);
    removed.out_has("kept branch main");
    assert!(!exists(&plain.path), "the worktree was not removed");
    assert!(
        s.has_branch(&s.main, "main"),
        "remove deleted the default branch"
    );

    let forced = worktree_on_main(&s, "forced");
    s.run(&["remove", "--force", "--delete-remote", &forced.path])
        .rc(0);
    assert!(!exists(&forced.path), "the worktree was not removed");
    assert!(
        s.has_branch(&s.main, "main"),
        "--force deleted the default branch"
    );
    assert!(
        s.has_branch(&bare, "main"),
        "--delete-remote deleted the default branch on the remote"
    );
}

// Ahead of its remote: nothing is lost by keeping the branch, so there is nothing to refuse,
// and no hint to delete it with -D.
#[test]
fn an_unpushed_default_branch_is_kept_not_refused() {
    let s = Sandbox::with_project();
    let wt = worktree_on_main(&s, "ahead");
    s.commit_file(&wt.path, "ahead", "not pushed");
    let tip = s.git(&wt.path, &["rev-parse", "HEAD"]);

    let status = s.run(&["status", "--json", &wt.path]);
    status.rc(0);
    status.out_has(r#""verdict": "removable""#);

    let removed = s.run(&["remove", &wt.path]);
    removed.rc(0);
    assert!(
        !removed.err.contains("branch -D") && !removed.out.contains("branch -D"),
        "the hint offers to delete the default branch: {}{}",
        removed.out,
        removed.err
    );
    assert_eq!(
        s.git(&s.main, &["rev-parse", "refs/heads/main"]),
        tip,
        "the default branch lost its commit"
    );
}

// The remote's HEAD names the default branch, whatever it is called. With no remote HEAD
// to read, the conventional names stand in for it.
#[test]
fn the_default_branch_is_what_the_remote_head_names() {
    let s = Sandbox::with_project();
    s.git(&s.main, &["push", "-q", "origin", "main:trunk"]);
    s.git(&s.main, &["fetch", "-q", "origin"]);
    s.git(&s.main, &["remote", "set-head", "origin", "trunk"]);
    let trunk = format!("{}/wt-trunk", s.t);
    s.git(
        &s.main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "trunk",
            &trunk,
            "origin/trunk",
        ],
    );
    s.run(&["remove", &trunk]).rc(0);
    assert!(
        s.has_branch(&s.main, "trunk"),
        "the remote's default branch was deleted"
    );

    // Configured elsewhere, so that main stands only by its name.
    s.git(&s.main, &["remote", "set-head", "origin", "--delete"]);
    s.git(&s.main, &["config", "init.defaultBranch", "develop"]);
    let fallback = worktree_on_main(&s, "fallback");
    s.run(&["remove", &fallback.path]).rc(0);
    assert!(
        s.has_branch(&s.main, "main"),
        "with no remote HEAD, main was deleted"
    );

    s.git(&s.main, &["checkout", "-q", "main"]);
    let develop = s.worktree("develop");
    s.run(&["remove", &develop.path]).rc(0);
    assert!(
        s.has_branch(&s.main, "develop"),
        "with no remote HEAD, the configured default branch was deleted"
    );

    // Not every branch is the default one: an ordinary merged branch still goes.
    s.git(&s.main, &["checkout", "-q", "main"]);
    let ordinary = s.worktree("ordinary");
    s.run(&["remove", &ordinary.path]).rc(0);
    assert!(
        !s.has_branch(&s.main, "ordinary"),
        "a merged branch was kept"
    );
}
