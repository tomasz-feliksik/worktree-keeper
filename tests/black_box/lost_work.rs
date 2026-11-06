//! States where a removal could lose work, or lose the wrong work: what git's defaults and
//! the caller's environment hide from a check.

use crate::support::{Ran, Sandbox, append, exists, ran, rm, write};

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
