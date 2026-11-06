//! `actor`, `provision` and `create`.

use crate::support::{
    Sandbox, append, chmod, exists, is_file, mkdir, read, rm, rm_rf, running_as_root, write,
};

#[test]
fn actor() {
    let s = Sandbox::with_project();
    let wt = s.worktree("feat-actor");
    let linked = s.run(&["actor", &wt.path]);
    linked.rc(0);
    assert_eq!(linked.out, "wt-feat-actor", "linked worktree actor");
    assert_eq!(
        s.run(&["actor", &wt.file("sub")]).out,
        "",
        "a submodule is not a worktree"
    );
    assert_eq!(
        s.run(&["actor", &s.main]).out,
        "",
        "main checkout must have no actor"
    );
    let outside = s.run(&["actor", &s.root]);
    outside.rc(0);
    assert_eq!(outside.out, "", "outside git must have no actor");
}

#[test]
fn provision_noops() {
    let s = Sandbox::new();
    let t = &s.t;
    let repo = format!("{t}/bare");
    let wt = format!("{repo}-wt");
    mkdir(&repo);
    s.git(&repo, &["init", "-q"]);
    s.git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    s.git(&repo, &["worktree", "add", "-q", &wt]);
    let provision = s.run(&["provision", &wt]);
    provision.rc(0);
    assert_eq!(
        provision.out, "",
        "provision said something where nothing applies"
    );
}

// The same absolute path from anywhere in the repository, beside the main checkout.
#[test]
fn create_places_and_provisions() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let want = format!("{t}/project.worktrees/feat-place");
    assert_eq!(s.create("feat/place"), want);
    assert!(
        exists(format!("{want}/sub/nested/.git")),
        "create did not provision nested submodules"
    );
    let below = format!("{main}/below");
    mkdir(&below);
    let from_below = s.run_in(&below, &["create", "from-below"]);
    from_below.rc(0);
    assert_eq!(from_below.out, format!("{t}/project.worktrees/from-below"));
    assert!(
        is_file(format!("{}/README", from_below.out)),
        "from a subdirectory: nothing was checked out"
    );
    let from_worktree = s.run_in(&want, &["create", "from-worktree"]);
    from_worktree.rc(0);
    assert_eq!(
        from_worktree.out,
        format!("{t}/project.worktrees/from-worktree")
    );
    assert!(
        !exists(format!("{below}/project.worktrees"))
            && !exists(format!("{want}/project.worktrees")),
        "placed relative to the current directory"
    );
}

// Only a checkout has a place beside it. The parent of a submodule's git directory is inside
// .git, and a bare repository's is unrelated, so both refuse and create nothing.
#[test]
fn create_refuses_what_is_not_a_checkout() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let refusal = "is not the .git directory of a checkout";
    let in_submodule = s.run_in(&format!("{main}/sub"), &["create", "in-submodule"]);
    in_submodule.rc(1);
    in_submodule.err_has(refusal);
    assert_eq!(in_submodule.out, "", "a refused create printed a path");
    assert!(
        !exists(format!("{main}/.git/modules.worktrees")),
        "created inside the git directory"
    );
    let bare = format!("{t}/project.git");
    let in_bare = s.run_in(&bare, &["create", "in-bare"]);
    in_bare.rc(1);
    in_bare.err_has(refusal);
    assert!(
        !exists(format!("{t}.worktrees")),
        "created beside the wrong folder"
    );
    assert!(
        !s.has_branch(&bare, "in-bare"),
        "a refused create made a branch"
    );
}

// Absolute links break if the repository and its worktrees move together, so create says so,
// on stderr, because callers read stdout as the path. Set to true, it says nothing.
#[test]
fn create_notes_absolute_links() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let note = "worktree.useRelativePaths is not true";
    let absolute = s.run_in(main, &["create", "absolute"]);
    absolute.rc(0);
    absolute.err_has(note);
    assert_eq!(
        absolute.out,
        format!("{t}/project.worktrees/absolute"),
        "stdout is not only the path"
    );
    s.git(main, &["config", "worktree.useRelativePaths", "false"]);
    s.run_in(main, &["create", "turned-off"]).err_has(note);
    s.git(main, &["config", "worktree.useRelativePaths", "true"]);
    let relative = s.run_in(main, &["create", "relative"]);
    relative.rc(0);
    assert!(
        !relative.err.contains("useRelativePaths"),
        "noted a setting that is on: {}",
        relative.err
    );
    assert!(
        !read(format!("{}/.git", relative.out)).starts_with("gitdir: /"),
        "setup: .git is not relative"
    );
}

// The directory goes with its last worktree, and only then. Anything in it keeps it, even
// something the user put there. A failed create leaves nothing behind either.
#[test]
fn remove_tidies_the_empty_directory() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let dir = format!("{t}/project.worktrees");
    s.git(main, &["config", "worktree.useRelativePaths", "true"]);
    let one = s.create("one");
    let two = s.create("two");
    let removed = s.run(&["remove", "--keep-branch", &one]);
    removed.rc(0);
    assert!(exists(&two), "removing one worktree touched the other");
    assert!(
        !removed.out.contains("removed empty"),
        "claimed to remove a directory still in use: {}",
        removed.out
    );
    write(format!("{dir}/notes"), "mine\n");
    let removed = s.run(&["remove", "--keep-branch", &two]);
    removed.rc(0);
    assert!(
        is_file(format!("{dir}/notes")),
        "removal deleted what the user put in the directory"
    );
    assert!(
        !removed.out.contains("could not remove"),
        "warned about a directory that is simply not empty: {}",
        removed.out
    );
    rm(format!("{dir}/notes"));
    let three = s.create("three");
    let removed = s.run(&["remove", "--keep-branch", &three]);
    removed.rc(0);
    removed.out_has(&format!("removed empty {dir}"));
    assert!(!exists(&dir), "the empty directory stayed");

    // A directory that cannot be removed warns, and the removal has still succeeded.
    if !running_as_root(&s) {
        let stuck = s.create("stuck");
        chmod(t, 0o555);
        let removed = s.run(&["remove", "--keep-branch", &stuck]);
        chmod(t, 0o755);
        removed.rc(0);
        removed.out_has(&format!("could not remove {dir}"));
        assert!(!exists(&stuck), "the worktree was not removed");
        rm_rf(&dir);
    }

    s.git(main, &["branch", "taken"]);
    s.git(
        main,
        &[
            "worktree",
            "add",
            "-q",
            &format!("{t}/holds-taken"),
            "taken",
        ],
    );
    s.run_in(main, &["create", "taken"]).rc(1);
    assert!(!exists(&dir), "a failed create left the directory behind");
}

// workmux's post_create is fail-fast, so a failed `direnv allow` warns and exits 0, and
// create still hands back the path. A path that is not a directory stays exit 1.
#[test]
fn provision_never_fails_its_caller() {
    let s = Sandbox::with_project();
    let (t, main, root) = (&s.t, &s.main, &s.root);
    let wt = format!("{t}/wt-failing");
    write(format!("{main}/.envrc"), "export FOO=1\n");
    s.git(main, &["add", ".envrc"]);
    s.git(main, &["commit", "-q", "-m", "envrc"]);
    s.git(main, &["worktree", "add", "-q", "-b", "failing", &wt]);
    direnv(&s, main, "allow");
    // The main checkout stays allowed, and no new allowance can be written: a real failure,
    // so it works on a wrapped binary too, whose own direnv comes ahead of anything on PATH.
    let allowances = format!("{root}/xdg-data/direnv/allow");
    chmod(&allowances, 0o555);
    let provision = s.run(&["provision", &wt]);
    provision.rc(0);
    if !running_as_root(&s) {
        provision.out_has("FAILED");
        assert!(
            !provision.out.contains("worktree .envrc allowed"),
            "a failed direnv allow claimed trust: {}",
            provision.out
        );
    }
    let created = s.run_in(main, &["create", "failing-create"]);
    chmod(&allowances, 0o755);
    created.rc(0);
    assert_eq!(
        created.out,
        format!("{t}/project.worktrees/failing-create"),
        "create did not hand back the path"
    );
    let missing = s.run(&["provision", &format!("{t}/no-such-directory")]);
    missing.rc(1);
    missing.err_has("is not a directory");
}

// feat/x and feat-x share a slug. Asking for the second must refuse, naming both, rather than
// hand back a worktree on the first. Asking again for the first still returns its path; a
// directory that is no worktree at all is refused too.
#[test]
fn create_refuses_a_slug_collision() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let first = format!("{t}/project.worktrees/feat-x");
    assert_eq!(s.create("feat/x"), first);
    let collision = s.run_in(main, &["create", "feat-x"]);
    collision.rc(1);
    collision.err_has("feat/x");
    collision.err_has("feat-x");
    assert_eq!(collision.out, "", "a refused create printed a path");
    let again = s.run_in(main, &["create", "feat/x"]);
    again.rc(0);
    assert_eq!(
        again.out, first,
        "the same branch did not get its path back"
    );
    mkdir(format!("{t}/project.worktrees/not-git"));
    let not_a_worktree = s.run_in(main, &["create", "not/git"]);
    not_a_worktree.rc(1);
    not_a_worktree.err_has("not a worktree");
    assert_eq!(not_a_worktree.out, "", "a refused create printed a path");
}

fn direnv(s: &Sandbox, dir: &str, verb: &str) {
    let output = s
        .command("direnv")
        .arg(verb)
        .arg(".")
        .current_dir(dir)
        .output()
        .expect("direnv should start: without it the test environment is incomplete");
    assert!(
        output.status.success(),
        "direnv {verb} in {dir}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Strict because most of direnv_gate's checks expect "not allowed". A status that failed, or
// that found no .envrc, must not read as that answer.
fn direnv_allowed(s: &Sandbox, dir: &str) -> bool {
    let output = s
        .command("direnv")
        .arg("status")
        .current_dir(dir)
        .output()
        .expect("direnv should start: without it the test environment is incomplete");
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "direnv status in {dir} failed: {status}"
    );
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("Found RC path ")),
        "direnv status in {dir} found no .envrc: {status}"
    );
    status
        .lines()
        .any(|line| line == "Found RC allowed 0" || line == "Found RC allowed true")
}

#[test]
fn direnv_gate() {
    let s = Sandbox::new();
    let t = &s.t;
    let repo = format!("{t}/envrepo");
    let wt = |letter: &str| format!("{repo}-{letter}");
    let add = |path: &str| s.git(&repo, &["worktree", "add", "-q", path]);
    mkdir(&repo);
    s.git(&repo, &["init", "-q"]);
    write(format!("{repo}/.envrc"), "export FOO=1\n");
    s.git(&repo, &["add", ".envrc"]);
    s.git(&repo, &["commit", "-q", "-m", "envrc"]);

    add(&wt("a"));
    s.run(&["provision", &wt("a")]);
    assert!(
        !direnv_allowed(&s, &wt("a")),
        "worktree trusted while the root .envrc was not allowed"
    );

    direnv(&s, &repo, "allow");
    add(&wt("b"));
    let provision = s.run(&["provision", &wt("b")]);
    assert!(
        direnv_allowed(&s, &wt("b")),
        "worktree not trusted although byte-identical to an allowed root"
    );
    provision.out_has("worktree .envrc allowed");

    // Never further than the root's own trust. A branch whose .envrc differs from the allowed
    // root copy is not trusted: those bytes were never read.
    s.git(&repo, &["switch", "-q", "-c", "other-envrc"]);
    append(format!("{repo}/.envrc"), "export UNREVIEWED=1\n");
    s.git(&repo, &["commit", "-q", "-am", "different envrc"]);
    s.git(&repo, &["switch", "-q", "-"]);
    s.git(&repo, &["worktree", "add", "-q", &wt("c"), "other-envrc"]);
    // The message proves this gate is what refused: anything else stopping the allow, such
    // as a failed `direnv status` in the tool, would leave the worktree untrusted too.
    let different = s.run(&["provision", &wt("c")]);
    different.rc(0);
    different.out_has("differs from the allowed root copy");
    assert!(
        !direnv_allowed(&s, &wt("c")),
        "worktree trusted although its .envrc differs from the allowed root copy"
    );

    // A root allow gone stale, the root .envrc edited since it was allowed, is no allow at
    // all, even for a worktree matching the root's OLD bytes.
    append(format!("{repo}/.envrc"), "export EDITED=1\n");
    add(&wt("d"));
    s.run(&["provision", &wt("d")]);
    assert!(
        !direnv_allowed(&s, &wt("d")),
        "worktree trusted on a stale root allow"
    );

    // An explicitly denied root is not allowed either.
    s.git(&repo, &["checkout", "-q", "--", ".envrc"]);
    direnv(&s, &repo, "deny");
    add(&wt("e"));
    s.run(&["provision", &wt("e")]);
    assert!(
        !direnv_allowed(&s, &wt("e")),
        "worktree trusted although the root .envrc is denied"
    );
}
