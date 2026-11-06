//! `status`, `check`, in-use detection, locks and integrated branches: what the tool says,
//! and that saying it writes nothing.

use crate::support::{
    Sandbox, append, exists, in_order, is_dir, is_file, mkdir, paths_under, ran, rm, tree_state,
    write,
};

// check changes nothing on disk, even where guard would clear metadata. The file is touched
// first, so that a plain `git status` would rewrite the index.
#[test]
fn check_changes_nothing() {
    let s = Sandbox::with_project();
    let wt = s.worktree("asked");
    touch(&wt.file("README"));
    touch(&wt.file("sub/README"));
    let before = tree_state(&[&wt.path, &wt.gitdir]);
    let check = s.run(&["check", &wt.path]);
    check.rc(0);
    check.out_has("may be removed");
    assert_eq!(
        tree_state(&[&wt.path, &wt.gitdir]),
        before,
        "check changed the disk"
    );
    s.run(&["guard", &wt.path]).rc(0);
    assert!(
        !is_dir(wt.git_file("modules")),
        "setup: guard should have cleared what check left"
    );
}

// status changes nothing either, and that includes a stranded worktree it has to verify.
#[test]
fn status_changes_nothing() {
    let s = Sandbox::with_project();
    let wt = s.worktree("looked");
    touch(&wt.file("README"));
    touch(&wt.file("sub/README"));
    let before = tree_state(&[&wt.path, &wt.gitdir]);
    s.run(&["status", &wt.path]).rc(0);
    assert_eq!(
        tree_state(&[&wt.path, &wt.gitdir]),
        before,
        "status changed the disk"
    );
}

// A later mtime for a file whose content stays the same.
fn touch(path: &str) {
    let content = std::fs::read(path).expect("the file should be readable");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(path, content).expect("the file should be written");
}

// check and guard give the same answer in every state, because they are the same code.
#[test]
fn check_agrees_with_guard() {
    let s = Sandbox::with_project();
    for state in ["clean", "dirty", "dirty_sub", "unpushed", "main"] {
        let wt = s.worktree(&format!("agree-{state}"));
        let mut path = wt.path.clone();
        match state {
            "dirty" => write(wt.file("scratch"), "x\n"),
            "dirty_sub" => append(wt.file("sub/README"), "x\n"),
            "unpushed" => s.commit_in_submodule(&wt),
            "main" => path.clone_from(&s.main),
            _ => {}
        }
        let check = s.run(&["check", &path]);
        let guard = s.run(&["guard", &path]);
        assert_eq!(check.rc, guard.rc, "{state}: check and guard disagree");
        assert!(
            state == "clean" || check.rc != 0,
            "{state}: check said removable"
        );
    }
    let garbage = s.worktree("agree-garbage");
    s.git(&garbage.path, &["submodule", "-q", "deinit", "-f", "--all"]);
    write(garbage.git_file("modules/sub/HEAD"), "garbage\n");
    let check = s.run(&["check", &garbage.path]);
    check.rc(3);
    check.err_has("cannot inspect");
}

// A process inside the worktree is named and refuses removal, under --force too. The
// caller's own process and its ancestors do not count. --ignore-in-use lets through.
#[test]
fn in_use_refuses_and_names_the_process() {
    let s = Sandbox::with_project();
    let wt = s.worktree("busy");
    let holder = s.holder_in(&wt.path);
    let pid = holder.pid();
    for sub in ["check", "guard"] {
        let plain = s.run(&[sub, &wt.path]);
        plain.rc(1);
        plain.err_has(&format!("in use by sleep (pid {pid})"));
        let forced = s.run(&[sub, "--force", &wt.path]);
        forced.rc(1);
        forced.err_has("in use by sleep");
    }
    let removed = s.run(&["remove", "--force", &wt.path]);
    removed.rc(1);
    removed.err_has("in use by sleep");
    assert!(
        exists(wt.file("sub/README")),
        "a refused removal emptied the submodule"
    );
    let status = s.run(&["status", &wt.path]);
    status.rc(0);
    status.out_has("refused");
    status.out_has("in_use");
    s.run(&["check", "--ignore-in-use", &wt.path]).rc(0);
    drop(holder);

    // The caller's own shell, sitting in the worktree, is not a holder. The second command
    // keeps bash alive: given only one, it would exec the tool and stop being an ancestor.
    let from_inside = ran(s
        .command("bash")
        .current_dir(&wt.path)
        .env("WORKTREE", s.tool().get_program())
        .args(["-c", r#""$WORKTREE" check "$PWD"; exit $?"#]));
    from_inside.rc(0);
    s.run(&["remove", &wt.path]).rc(0);
}

// --ignore-in-use removes without asking, and names what it left running. It covers running
// processes and nothing else, so uncommitted changes still refuse.
#[test]
fn ignore_in_use_names_what_it_left_running() {
    let s = Sandbox::with_project();
    let wt = s.worktree("busy");
    let mut holder = s.holder_in(&wt.path);
    let running = format!("still running: sleep (pid {})", holder.pid());
    write(wt.file("dirt"), "dirt\n");
    let dirty = s.run(&["remove", "--ignore-in-use", &wt.path]);
    dirty.rc(1);
    dirty.err_has("uncommitted changes");
    rm(wt.file("dirt"));
    // A hook runs guard alone and its tool removes the worktree, so guard reports too.
    let guard = s.run(&["guard", "--ignore-in-use", &wt.path]);
    guard.rc(0);
    guard.out_has(&running);
    let removed = s.run(&["remove", "--ignore-in-use", &wt.path]);
    removed.rc(0);
    removed.out_has(&running);
    assert!(!exists(&wt.path), "the worktree was not removed");
    assert!(
        holder.is_running(),
        "the holder should have been left running"
    );
    drop(holder);

    let idle = s.worktree("idle");
    let removed = s.run(&["remove", "--ignore-in-use", &idle.path]);
    removed.rc(0);
    assert!(
        !removed.out.contains("still running"),
        "reported a process when there was none: {}",
        removed.out
    );
}

// A locked worktree is refused naming the reason, under --force too, until unlocked.
#[test]
fn lock_refuses_until_unlocked() {
    let s = Sandbox::with_project();
    let wt = s.worktree("held");
    let reason = "agent-7 is working here";
    s.run(&["lock", "--reason", reason, &wt.path]).rc(0);
    assert!(
        is_file(wt.git_file("locked")),
        "lock did not use git worktree lock"
    );
    for sub in ["check", "guard"] {
        let forced = s.run(&[sub, "--force", &wt.path]);
        forced.rc(1);
        forced.err_has(reason);
    }
    let removed = s.run(&["remove", "--force", &wt.path]);
    removed.rc(1);
    removed.err_has(reason);
    assert!(
        exists(wt.file("sub/README")),
        "a refused removal emptied the submodule"
    );
    s.run(&["lock", &wt.path]).rc(1);
    s.run(&["unlock", &wt.path]).rc(0);
    assert!(!is_file(wt.git_file("locked")), "unlock did not release it");
    s.run(&["remove", &wt.path]).rc(0);
    let main = s.run(&["unlock", &s.main]);
    main.rc(1);
    main.err_has("main worktree");
}

// Squashed or rebased, the branch's content is in the target although its commits are not,
// so it goes without --force. Anything the target lacks keeps it, with nothing changed.
#[test]
fn integrated_branch_is_deleted() {
    let s = Sandbox::with_project();
    let main = &s.main;
    let squash = |branch: &str| {
        s.git(main, &["merge", "-q", "--squash", branch]);
        s.git(main, &["commit", "-q", "-m", "squashed"]);
    };

    let squashed = s.worktree("squashed");
    s.commit_file(&squashed.path, "one", "1");
    s.commit_file(&squashed.path, "two", "2");
    squash("squashed");
    let removed = s.run(&["remove", &squashed.path]);
    removed.rc(0);
    removed.out_has("squash-merged or rebased");
    assert!(
        !exists(&squashed.path),
        "a squash-merged worktree was not removed"
    );
    assert!(
        !s.has_branch(main, "squashed"),
        "a squash-merged branch was kept"
    );

    let rebased = s.worktree("rebased");
    s.commit_file(&rebased.path, "three", "3");
    s.commit_file(main, "elsewhere", "x");
    s.git(main, &["cherry-pick", "rebased"]);
    s.run(&["remove", &rebased.path]).rc(0);
    assert!(!s.has_branch(main, "rebased"), "a rebased branch was kept");

    // With an upstream it is not ancestor of, squashed into the main checkout all the same.
    let upstreamed = s.worktree("upstreamed");
    s.commit_file(&upstreamed.path, "pushed", "p");
    s.git(
        &upstreamed.path,
        &["push", "-q", "-u", "origin", "upstreamed"],
    );
    s.commit_file(&upstreamed.path, "unpushed", "u");
    squash("upstreamed");
    s.run(&["remove", &upstreamed.path]).rc(0);
    assert!(
        !s.has_branch(main, "upstreamed"),
        "a branch squashed into the main checkout was kept"
    );

    // One more commit after the squash: the target lacks it.
    let added_to = s.worktree("added-to");
    s.commit_file(&added_to.path, "four", "4");
    squash("added-to");
    s.commit_file(&added_to.path, "five", "5");
    let refused = s.run(&["remove", &added_to.path]);
    refused.rc(1);
    refused.err_has("not merged");
    assert!(
        is_file(added_to.file("five")),
        "unintegrated work was removed"
    );
    assert!(
        s.has_branch(main, "added-to"),
        "a branch with unintegrated work was deleted"
    );

    // Squashed, then reverted: merging would bring it back, so the target lacks it.
    let reverted = s.worktree("reverted");
    s.commit_file(&reverted.path, "six", "6");
    squash("reverted");
    s.git(main, &["revert", "--no-edit", "HEAD"]);
    s.run(&["remove", &reverted.path]).rc(1);
    assert!(
        s.has_branch(main, "reverted"),
        "a reverted branch was deleted"
    );

    // The target has moved on over the same lines: a conflict cannot show the content is there.
    let conflicting = s.worktree("conflicting");
    s.commit_file(&conflicting.path, "seven", "7");
    squash("conflicting");
    write(format!("{main}/seven"), "changed\n");
    s.git(main, &["commit", "-q", "-am", "seven"]);
    s.run(&["remove", &conflicting.path]).rc(1);
    assert!(
        s.has_branch(main, "conflicting"),
        "a branch that conflicts was deleted"
    );

    // --keep-branch keeps it, whatever the state.
    let kept = s.worktree("kept");
    s.commit_file(&kept.path, "eight", "8");
    squash("kept");
    s.run(&["remove", "--keep-branch", &kept.path]).rc(0);
    assert!(
        s.has_branch(main, "kept"),
        "--keep-branch deleted an integrated branch"
    );
}

// A question writes no objects, though merge-tree writes them. A colon in the path must not
// split the list of object directories it reads from.
#[test]
fn status_of_an_integrated_branch_writes_nothing() {
    let s = Sandbox::with_project();
    let t = &s.t;
    let main = format!("{t}/co:lon");
    let objects = format!("{main}/.git/objects");
    s.git(
        t,
        &["clone", "-q", "--recurse-submodules", "project.git", &main],
    );
    let asked = format!("{t}/wt-asked");
    s.git(&main, &["worktree", "add", "-q", "-b", "asked", &asked]);
    s.commit_file(&asked, "one", "1");
    s.git(&main, &["merge", "-q", "--squash", "asked"]);
    s.git(&main, &["commit", "-q", "-m", "squashed"]);
    let before = paths_under(&objects);
    let status = s.run(&["status", "--json", &asked]);
    status.rc(0);
    status.out_has(r#""merged": true"#);
    status.out_has(r#""verdict": "removable""#);
    assert_eq!(paths_under(&objects), before, "status wrote objects");

    // Not integrated is the case with a new tree to write: the merge would change something.
    let unmerged = format!("{t}/wt-unmerged");
    s.git(
        &main,
        &["worktree", "add", "-q", "-b", "unmerged", &unmerged],
    );
    s.commit_file(&unmerged, "two", "2");
    s.commit_file(&main, "elsewhere", "x");
    let before = paths_under(&objects);
    let status = s.run(&["status", "--json", &unmerged]);
    status.rc(0);
    status.out_has(r#""merged": false"#);
    assert_eq!(paths_under(&objects), before, "status wrote objects");
}

// status reads each clone for commits no remote has, and refuses on them.
#[test]
fn status_reports_unpushed_commits() {
    let s = Sandbox::with_project();
    let wt = s.worktree("holding");
    let clean = s.run(&["status", "--json", &wt.path]);
    clean.rc(0);
    clean.out_has(r#""unpushed": false"#);
    clean.out_has(r#""verdict": "removable""#);
    s.commit_in_submodule(&wt);
    let holding = s.run(&["status", "--json", &wt.path]);
    holding.rc(0);
    holding.out_has(r#""unpushed": true"#);
    holding.out_has(r#""kind": "unpushed_submodule""#);
}

// Every worktree of the repository gets a verdict, wherever it lives; the main checkout is
// not one. An orphan is unknown, never safe, and makes the exit status 3. Nothing is
// deleted. Outside a repository, with no path, there is nothing to look at.
#[test]
fn status_lists_every_worktree() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let dir = format!("{t}/project.worktrees");
    let elsewhere = format!("{t}/elsewhere/placed-by-another-tool");
    let safe = s.create("safe-one");
    let busy = s.create("dirty-one");
    s.git(main, &["worktree", "add", "-q", "-b", "other", &elsewhere]);
    write(format!("{busy}/scratch"), "x\n");
    let status = s.run_in(main, &["status"]);
    status.rc(0);
    let out = &status.out;
    assert!(
        in_order(out, &["removable", &safe]),
        "the clean worktree is not removable: {out}"
    );
    assert!(
        in_order(out, &["refused", &busy, "dirty"]),
        "the dirty worktree is not refused: {out}"
    );
    assert!(
        out.contains(&elsewhere),
        "a worktree outside the directory is missing: {out}"
    );
    assert_eq!(
        out.lines().count(),
        3,
        "expected three worktrees and no main checkout: {out}"
    );

    let orphan = format!("{dir}/orphan");
    mkdir(&orphan);
    write(format!("{orphan}/.git"), &format!("gitdir: {t}/nowhere\n"));
    let status = s.run_in(&safe, &["status"]);
    status.rc(3);
    assert!(
        in_order(&status.out, &["unknown", &orphan]),
        "an orphan is not unknown: {}",
        status.out
    );
    assert!(
        is_file(format!("{busy}/scratch")) && is_dir(&orphan),
        "status deleted something"
    );

    let named = s.run(&["status", &dir]);
    named.rc(3);
    named.out_has(&safe);
    named.out_has(&orphan);

    let outside = s.run(&["status"]);
    outside.rc(1);
    outside.err_has("not inside a git repository");
}
