//! `sweep`: what is left over, what it deletes, and what it never touches.

use crate::support::{Sandbox, exists, is_dir, is_file, mkdir, read, rm, rm_rf, write};

#[test]
fn sweep() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let root = format!("{t}/root");
    let gone = format!("{root}/project/gone-worktree");
    let group = format!("{root}/empty-group");
    let orphan = format!("{root}/project/orphan");
    let live = format!("{root}/project/live");
    mkdir(format!("{gone}/.vite"));
    mkdir(&group);
    write(format!("{gone}/.vite/x"), "cache\n");
    s.git(main, &["worktree", "add", "-q", "-b", "live", &live]);
    mkdir(&orphan);
    write(format!("{orphan}/.git"), &format!("gitdir: {t}/nowhere\n"));
    write(format!("{orphan}/work"), "precious\n");

    let dry = s.run(&["sweep", &root]);
    dry.rc(0);
    dry.out_has(&format!("leftover {gone}"));
    dry.out_has(&format!("leftover {group}"));
    dry.out_has(&format!("orphan   {orphan}"));
    assert!(is_dir(&gone), "dry run deleted a leftover");
    dry.out_has("dry run");

    let applied = s.run(&["sweep", "--apply", &root]);
    applied.rc(0);
    assert!(
        !applied.out.contains("dry run"),
        "--apply called itself a dry run: {}",
        applied.out
    );
    assert!(!exists(&gone), "--apply did not delete the leftover");
    assert!(!exists(&group), "--apply did not delete the empty group");
    assert!(
        is_file(format!("{orphan}/work")),
        "--apply deleted an orphan's files"
    );
    assert!(
        exists(format!("{live}/.git")),
        "--apply touched a live worktree"
    );
}

// With no directory named, sweep looks beside this repository, from anywhere in it, and
// outside one it refuses. --apply tidies the directory once nothing is left in it, and a dry
// run does not.
#[test]
fn sweep_defaults_to_this_repository() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let dir = format!("{t}/project.worktrees");
    mkdir(format!("{dir}/gone/.vite"));
    let live = s.create("live");
    let from_worktree = s.run_in(&live, &["sweep"]);
    from_worktree.rc(0);
    from_worktree.out_has(&format!("leftover {dir}/gone"));
    assert!(
        !from_worktree.out.contains(&live),
        "a live worktree was reported: {}",
        from_worktree.out
    );
    let applied = s.run_in(main, &["sweep", "--apply"]);
    applied.rc(0);
    assert!(
        !exists(format!("{dir}/gone")) && is_dir(&live),
        "--apply deleted the wrong things: {}",
        applied.out
    );

    s.run(&["remove", "--keep-branch", &live]).rc(0);
    mkdir(&dir);
    s.run_in(main, &["sweep"]);
    assert!(is_dir(&dir), "a dry run removed the directory");
    write(format!("{dir}/notes"), "mine\n");
    s.run_in(main, &["sweep", "--apply"]);
    assert!(
        is_file(format!("{dir}/notes")),
        "--apply deleted a file the user put there"
    );
    rm(format!("{dir}/notes"));
    s.run_in(main, &["sweep", "--apply"]).rc(0);
    assert!(!exists(&dir), "--apply left the empty directory");

    // Only a directory named as this tool names one is tidied.
    let (kept, other) = (format!("{t}/kept"), format!("{t}/other.worktrees"));
    mkdir(&kept);
    mkdir(&other);
    s.run(&["sweep", "--apply", &kept, &other]).rc(0);
    assert!(
        is_dir(&kept),
        "--apply removed an empty directory it does not own"
    );
    assert!(
        !exists(&other),
        "--apply left a named empty worktrees directory"
    );

    let outside = s.run(&["sweep"]);
    outside.rc(1);
    outside.err_has("not inside a git repository");
}

// --repo with no directory looks beside each repository named, not beside the one the caller
// stands in, and works from outside any repository.
#[test]
fn sweep_repo_names_where_to_look() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let dir = format!("{t}/project.worktrees");
    let here = format!("{t}/here");
    let theirs = format!("{t}/here.worktrees/theirs");
    s.git(t, &["init", "-q", &here]);
    s.git(&here, &["commit", "-q", "--allow-empty", "-m", "init"]);
    mkdir(format!("{dir}/gone"));
    mkdir(&theirs);

    let named = s.run(&["sweep", "--repo", main]);
    named.rc(0);
    named.out_has(&format!("leftover {dir}/gone"));

    let from_here = s.run_in(&here, &["sweep", "--repo", main]);
    from_here.rc(0);
    from_here.out_has(&format!("leftover {dir}/gone"));
    assert!(
        !from_here.out.contains("theirs"),
        "looked beside the current repository: {}",
        from_here.out
    );

    // Two spellings of one repository are one directory, so the second delete has nothing to trip on.
    let spelled_twice = s.run(&[
        "sweep",
        "--apply",
        "--repo",
        main,
        "--repo",
        &format!("{main}/"),
    ]);
    spelled_twice.rc(0);
    assert!(
        !exists(&dir),
        "--apply left what it was pointed at: {}",
        spelled_twice.out
    );
    assert!(
        is_dir(&theirs),
        "--apply touched another repository's directory"
    );

    // A linked worktree names its repository.
    let live = s.create("live");
    mkdir(format!("{dir}/also-gone"));
    let through_worktree = s.run(&["sweep", "--repo", &live]);
    through_worktree.rc(0);
    through_worktree.out_has(&format!("leftover {dir}/also-gone"));

    // A repository with no place beside it is refused, and naming a directory still works.
    let bare = format!("{t}/bare.git");
    s.git(t, &["init", "-q", "--bare", &bare]);
    let refused = s.run(&["sweep", "--repo", &bare]);
    refused.rc(1);
    refused.err_has("not the .git directory of a checkout");
    let with_directory = s.run(&["sweep", "--repo", &bare, &format!("{t}/here.worktrees")]);
    with_directory.rc(0);
    with_directory.out_has(&format!("leftover {theirs}"));
}

// The flat layout, <repo>__worktrees/<name> with worktrees at depth 1, is swept the same way
// when named: the live worktree is kept, a leftover beside it is reported.
#[test]
fn sweep_flat_layout() {
    let s = Sandbox::with_project();
    let t = &s.t;
    let flat = format!("{t}/project__worktrees");
    let live = format!("{flat}/flat-live");
    let old = format!("{flat}/old-task");
    mkdir(&flat);
    s.git(
        &s.main,
        &["worktree", "add", "-q", "-b", "flat-live", &live],
    );
    mkdir(format!("{old}/.vite"));
    let dry = s.run(&["sweep", &flat]);
    dry.rc(0);
    dry.out_has(&format!("leftover {old}"));
    assert!(
        !dry.out.contains("flat-live"),
        "a live flat-layout worktree was reported: {}",
        dry.out
    );
    s.run(&["sweep", "--apply", &flat]);
    assert!(
        !exists(&old) && is_dir(&live),
        "--apply on the flat layout: wrong things deleted"
    );
}

// A registration whose directory is gone, holding an unpushed submodule clone, is never pruned.
#[test]
fn sweep_stale_registration() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let empty_root = format!("{t}/empty-root");
    let holding = s.worktree("stale-reg");
    s.commit_in_submodule(&holding);
    rm_rf(&holding.path);
    let applied = s.run(&["sweep", "--apply", "--repo", main, &empty_root]);
    applied.rc(0);
    applied.out_has("NOT pruning");
    assert!(
        is_dir(holding.git_file("modules/sub")),
        "the unpushed clone was pruned"
    );

    // And one with nothing unpushed IS pruned, even beside the blocked one.
    let clean = s.worktree("stale-clean");
    rm_rf(&clean.path);
    s.run(&["sweep", "--apply", "--repo", main, &empty_root]);
    assert!(
        !is_dir(&clean.gitdir),
        "a stale registration with nothing unpushed was not pruned"
    );
}

// A locked registration is left alone even when its directory is gone: it may be on removable
// media, and `git worktree prune` leaves it too.
#[test]
fn sweep_leaves_locked_registrations() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let wt = s.worktree("locked-reg");
    s.git(
        main,
        &[
            "worktree",
            "lock",
            "--reason",
            "on a removable disk",
            &wt.path,
        ],
    );
    rm_rf(&wt.path);
    let applied = s.run(&[
        "sweep",
        "--apply",
        "--repo",
        main,
        &format!("{t}/empty-root"),
    ]);
    applied.rc(0);
    assert!(is_dir(&wt.gitdir), "--apply pruned a locked registration");
    assert!(
        !applied.out.contains("pruned"),
        "reported pruning a locked registration: {}",
        applied.out
    );
}

// With worktree.useRelativePaths both pointers are relative, each to the directory holding
// it: the admin dir's gitdir and the worktree's .git file. A live worktree is neither stale
// nor orphan, wherever sweep runs from, and --apply leaves it working; a registration really
// gone is still pruned.
#[test]
fn sweep_relative_paths() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let root = format!("{t}/root");
    let live = format!("{root}/project/rel-live");
    let gone = format!("{root}/project/rel-gone");
    let admin = format!("{main}/.git/worktrees");
    mkdir(format!("{root}/project"));
    s.git(main, &["config", "worktree.useRelativePaths", "true"]);
    s.git(main, &["worktree", "add", "-q", "-b", "rel-live", &live]);
    s.git(main, &["worktree", "add", "-q", "-b", "rel-gone", &gone]);
    assert!(
        !read(format!("{admin}/rel-live/gitdir")).starts_with('/'),
        "setup: gitdir is not relative"
    );
    assert!(
        !read(format!("{live}/.git")).starts_with("gitdir: /"),
        "setup: .git is not relative"
    );
    rm_rf(&gone);

    let dry = s.run(&["sweep", "--repo", main, &root]);
    dry.rc(0);
    assert!(
        !dry.out.contains("rel-live"),
        "a live worktree was reported: {}",
        dry.out
    );
    dry.out_has(&format!("stale    {gone} "));

    let applied = s.run(&["sweep", "--apply", "--repo", main, &root]);
    applied.rc(0);
    assert!(
        !applied.out.contains("rel-live"),
        "--apply reported a live worktree: {}",
        applied.out
    );
    applied.out_has(&format!("pruned   {gone} "));
    assert!(
        !exists(format!("{admin}/rel-gone")),
        "--apply did not prune the gone registration"
    );
    assert!(
        is_dir(format!("{admin}/rel-live")),
        "--apply deleted a live worktree's admin dir"
    );
    assert!(
        s.git(main, &["worktree", "list"]).contains("rel-live"),
        "--apply unregistered a live worktree"
    );
    assert!(
        s.git_ok(&live, &["status"]),
        "--apply cut a live worktree off from its repository"
    );
}
