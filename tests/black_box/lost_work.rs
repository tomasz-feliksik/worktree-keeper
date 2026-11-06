//! States where a removal could lose work, or lose the wrong work: what git's defaults and
//! the caller's environment hide from a check.

use crate::support::{Ran, Sandbox, append, exists, ran, read, rm, tree_state, write};

/// The tool with these variables in its environment, as a git hook would run it.
fn run_with(s: &Sandbox, env: &[(&str, &str)], args: &[&str]) -> Ran {
    let mut command = s.tool();
    command.envs(env.iter().copied()).args(args);
    ran(&mut command)
}

// What `git worktree add` exports to its hooks. They outrank -C, so without clearing them
// a question about B is answered about A, and `remove B` deletes A.
#[test]
fn leaked_git_dir_and_work_tree_cannot_redirect_to_another_worktree() {
    let s = Sandbox::with_project();
    let (a, b) = (s.worktree("leak-a"), s.worktree("leak-b"));
    write(b.file("work"), "only copy\n");
    let env = [
        ("GIT_DIR", a.gitdir.as_str()),
        ("GIT_WORK_TREE", a.path.as_str()),
    ];

    let check = run_with(&s, &env, &["check", &b.path]);
    check.rc(1);
    check.err_has(&b.path);

    let remove = run_with(&s, &env, &["remove", &b.path]);
    remove.rc(1);
    assert!(exists(&a.path), "asked to remove B, and removed A");
    assert!(exists(b.file("work")), "the refused worktree lost its file");

    let clean = s.worktree("leak-clean");
    run_with(&s, &env, &["remove", &clean.path]).rc(0);
    assert!(!exists(&clean.path), "the worktree named was not removed");
    assert!(exists(&a.path), "removed the worktree in the environment");
}

// What pre-commit, post-commit and post-checkout export (measured on git 2.56): GIT_DIR
// alone. Git then takes the current directory as the work tree, which looks right, while
// the git directory it reports is the other worktree's. So its clones would be judged and
// cleared in place of the ones named.
#[test]
fn leaked_git_dir_alone_judges_the_clones_of_the_worktree_named() {
    let s = Sandbox::with_project();
    let (a, b) = (s.worktree("hook-a"), s.worktree("hook-b"));
    s.commit_in_submodule(&b);
    let index = a.git_file("index");
    let env = [
        ("GIT_DIR", a.gitdir.as_str()),
        ("GIT_INDEX_FILE", index.as_str()),
    ];

    let check = run_with(&s, &env, &["check", &b.path]);
    check.rc(1);
    check.err_has("holds commits no remote has");

    run_with(&s, &env, &["guard", &b.path]).rc(1);
    assert!(
        exists(a.git_file("modules/sub")),
        "a guard on B cleared A's submodule clones"
    );
    assert!(
        exists(b.git_file("modules/sub")),
        "the only copy of a commit was cleared"
    );
}

// `status.showUntrackedFiles = no` is an ordinary setting in a large repository. It changes
// what `git status` shows, and must not change what counts as safe to delete.
#[test]
fn a_config_hiding_untracked_files_hides_nothing_from_the_check() {
    let s = Sandbox::with_project();
    let wt = s.worktree("hidden");
    append(
        format!("{}/gitconfig", s.root),
        "[status]\n\tshowUntrackedFiles = no\n",
    );

    write(wt.file("work"), "only copy\n");
    let remove = s.run(&["remove", &wt.path]);
    remove.rc(1);
    remove.err_has("uncommitted changes");
    assert!(exists(wt.file("work")), "a refusal deleted the file");
    rm(wt.file("work"));

    write(wt.file("sub/work"), "only copy\n");
    let guard = s.run(&["guard", "--force", &wt.path]);
    guard.rc(1);
    guard.err_has("submodule sub");
    assert!(exists(wt.file("sub/work")), "a refusal deleted the file");
}

// A commit on a detached HEAD is held only by that HEAD and its reflog, and both go with the
// worktree. Holding it anywhere else makes the worktree removable again.
#[test]
fn a_commit_on_a_detached_head_is_refused_until_something_holds_it() {
    let s = Sandbox::with_project();
    let wt = s.worktree("detached");
    s.git(&wt.path, &["checkout", "-q", "--detach"]);
    s.run(&["check", &wt.path]).rc(0);

    s.commit_file(&wt.path, "work", "only copy");
    let check = s.run(&["check", &wt.path]);
    check.rc(1);
    check.err_has("detached HEAD");
    check.err_has("work");
    s.run(&["check", "--json", &wt.path])
        .out_has("\"detached_commits\"");
    s.run(&["status", "--json", &wt.path])
        .out_has("\"detached_commits\"");
    let remove = s.run(&["remove", &wt.path]);
    remove.rc(1);
    assert!(exists(wt.file("work")), "a refusal deleted the worktree");

    s.git(&wt.path, &["branch", "kept"]);
    s.run(&["remove", &wt.path]).rc(0);
    assert!(s.has_branch(&s.main, "kept"), "the branch holding it went");
}

// --force accepts losing it, as it accepts losing an unmerged branch.
#[test]
fn force_accepts_losing_a_commit_on_a_detached_head() {
    let s = Sandbox::with_project();
    let wt = s.worktree("detached-forced");
    s.git(&wt.path, &["checkout", "-q", "--detach"]);
    s.commit_file(&wt.path, "work", "only copy");
    s.run(&["remove", "--force", &wt.path]).rc(0);
    assert!(!exists(&wt.path), "--force did not remove it");
}

// A rebase stopped partway has a clean tree, a detached HEAD and new commits on neither the
// branch nor any other ref.
#[test]
fn a_commit_made_during_a_stopped_rebase_is_refused() {
    let s = Sandbox::with_project();
    let wt = s.worktree("rebasing");
    s.commit_file(&wt.path, "first", "one");
    assert!(
        !s.git_ok(&wt.path, &["rebase", "main", "-x", "exit 1"]),
        "setup: the rebase should have stopped"
    );
    assert_eq!(s.git(&wt.path, &["branch", "--show-current"]), "");
    s.run(&["check", &wt.path]).rc(0);

    s.commit_file(&wt.path, "second", "only copy");
    let check = s.run(&["check", &wt.path]);
    check.rc(1);
    check.err_has("detached HEAD");
}

// A tag or a remote holds a commit as surely as a branch does, and each is its own limb of
// the query, so each is shown alone.
#[test]
fn a_detached_commit_a_tag_or_remote_holds_is_removable() {
    let s = Sandbox::with_project();
    let tagged = s.worktree("held-by-tag");
    s.git(&tagged.path, &["checkout", "-q", "--detach"]);
    s.commit_file(&tagged.path, "work", "tagged");
    s.git(&tagged.path, &["tag", "kept"]);
    s.run(&["check", &tagged.path]).rc(0);

    let pushed = s.worktree("held-by-remote");
    s.git(&pushed.path, &["checkout", "-q", "--detach"]);
    s.commit_file(&pushed.path, "work", "pushed");
    s.git(
        &pushed.path,
        &["push", "-q", "origin", "HEAD:refs/heads/pushed"],
    );
    s.run(&["check", &pushed.path]).rc(0);
}

// `git status` does not look at a file flagged skip-worktree or assume-unchanged, so an edit
// under either flag has to be found another way. The check must not write while it does.
#[test]
fn an_edit_git_status_cannot_see_is_refused() {
    let s = Sandbox::with_project();
    for (name, flag) in [
        ("skip", "--skip-worktree"),
        ("assume", "--assume-unchanged"),
    ] {
        let wt = s.worktree(name);
        s.git(&wt.path, &["update-index", flag, "README"]);
        s.run(&["check", &wt.path]).rc(0);
        let same = read(wt.file("README"));
        write(wt.file("README"), &format!("{same}\n"));
        s.run(&["check", &wt.path]).rc(0);

        append(wt.file("README"), "only copy\n");
        let before = tree_state(&[&wt.path, &wt.gitdir]);
        let check = s.run(&["check", &wt.path]);
        check.rc(1);
        check.err_has("hidden: README");
        assert_eq!(
            tree_state(&[&wt.path, &wt.gitdir]),
            before,
            "{name}: the check changed the disk"
        );
        let remove = s.run(&["remove", &wt.path]);
        remove.rc(1);
        assert!(
            read(wt.file("README")).contains("only copy"),
            "{name}: a refusal lost the edit"
        );

        s.run(&["remove", "--force", &wt.path]).rc(0);
        assert!(!exists(&wt.path), "{name}: --force did not remove it");
    }
}

// The same in a submodule, which has its own index, and --force does not cover it.
#[test]
fn a_hidden_edit_in_a_submodule_is_refused_under_force() {
    let s = Sandbox::with_project();
    let wt = s.worktree("hidden-sub");
    s.git(
        &wt.file("sub"),
        &["update-index", "--skip-worktree", "README"],
    );
    append(wt.file("sub/README"), "only copy\n");
    let guard = s.run(&["guard", "--force", &wt.path]);
    guard.rc(1);
    guard.err_has("submodule sub");
    guard.err_has("hidden: README");
}

// Under a sparse checkout, a flagged file absent from disk is the normal state: nothing is
// there to lose, and refusing would make such a worktree impossible to remove.
#[test]
fn a_flagged_file_absent_from_disk_holds_no_work() {
    let s = Sandbox::with_project();
    let wt = s.worktree("sparse");
    s.git(&wt.path, &["update-index", "--skip-worktree", "README"]);
    rm(wt.file("README"));
    s.run(&["remove", &wt.path]).rc(0);
}
