//! Seed files: what `provision` copies from the main checkout, and what it never does.

use crate::support::{
    Sandbox, append, chmod, exists, is_symlink, mkdir, mode_of, read, rm, rm_rf, running_as_root,
    write,
};

// A project whose main branch declares .env and config/*.local, with both on disk, ignored,
// beside an ignored file nobody chose and a tracked file that matches the pattern.
fn seeded_project() -> Sandbox {
    let s = Sandbox::with_project();
    let main = &s.main;
    write(format!("{main}/.gitignore"), ".env\nconfig/*.local\n");
    write(
        format!("{main}/.worktreeinclude"),
        ".env\nconfig/*.local\ntracked.env\ndraft.txt\n",
    );
    write(format!("{main}/tracked.env"), "tracked\n");
    s.git(
        main,
        &["add", ".gitignore", ".worktreeinclude", "tracked.env"],
    );
    s.git(main, &["commit", "-q", "-m", "declare seed files"]);
    mkdir(format!("{main}/config"));
    write(format!("{main}/.env"), "secret\n");
    chmod(format!("{main}/.env"), 0o600);
    write(format!("{main}/config/app.local"), "local\n");
    write(format!("{main}/other.log"), "chosen-by-nobody\n");
    write(format!("{main}/draft.txt"), "listed-but-not-ignored\n");
    append(format!("{main}/.git/info/exclude"), "other.log\n");
    s
}

// A plain worktree: nothing has provisioned it yet.
fn add(s: &Sandbox, branch: &str) -> String {
    let t = &s.t;
    let path = format!("{t}/wt-{branch}");
    s.git(&s.main, &["worktree", "add", "-q", "-b", branch, &path]);
    path
}

#[test]
fn seed_copies_only_what_is_declared_and_ignored() {
    let s = seeded_project();
    let wt = add(&s, "seeded");
    let provision = s.run(&["provision", &wt]);
    provision.rc(0);
    provision.out_has("2 seed file");
    assert_eq!(read(format!("{wt}/.env")), "secret", ".env not copied");
    assert_eq!(
        mode_of(format!("{wt}/.env")),
        0o600,
        "a private file became readable"
    );
    assert_eq!(
        read(format!("{wt}/config/app.local")),
        "local",
        "a nested file was not copied"
    );
    assert!(
        !exists(format!("{wt}/other.log")),
        "copied an ignored file nobody declared"
    );
    assert!(
        !exists(format!("{wt}/draft.txt")),
        "copied a file git does not ignore"
    );
    assert_eq!(
        s.git(&wt, &["status", "--porcelain"]),
        "",
        "seeding dirtied the worktree"
    );
}

#[test]
fn seed_never_overwrites() {
    let s = seeded_project();
    let wt = add(&s, "seeded");
    write(format!("{wt}/.env"), "mine\n");
    s.run(&["provision", &wt]).rc(0);
    assert_eq!(
        read(format!("{wt}/.env")),
        "mine",
        "seeding overwrote an existing file"
    );
    assert_eq!(
        read(format!("{wt}/config/app.local")),
        "local",
        "one existing file stopped the rest"
    );
}

// The list comes from the main checkout: a branch cannot widen what is copied into it.
#[test]
fn seed_ignores_a_list_the_branch_changed() {
    let s = seeded_project();
    let wt = add(&s, "seeded");
    write(format!("{wt}/.worktreeinclude"), "other.log\n");
    s.git(&wt, &["commit", "-q", "-am", "ask for more"]);
    s.run_in(&wt, &["provision", "."]).rc(0);
    assert!(
        !exists(format!("{wt}/other.log")),
        "a branch's own list was obeyed"
    );
    assert_eq!(
        read(format!("{wt}/.env")),
        "secret",
        "the main checkout's list was not obeyed"
    );
}

// A branch can commit `config` as a symlink. Following it would write outside the worktree.
#[test]
fn seed_does_not_write_through_a_symlink() {
    let s = seeded_project();
    let t = &s.t;
    let outside = format!("{t}/outside");
    mkdir(&outside);
    let wt = format!("{t}/wt-seeded");
    s.git(
        &s.main,
        &[
            "worktree",
            "add",
            "-q",
            "--no-checkout",
            "-b",
            "seeded",
            &wt,
        ],
    );
    // Not recursing: this worktree's submodule clones do not exist yet, so under
    // submodule.recurse the checkout would fail halfway.
    s.git(&wt, &["-c", "submodule.recurse=false", "checkout", "-q"]);
    std::os::unix::fs::symlink(&outside, format!("{wt}/config"))
        .expect("the symlink should be made");
    let provision = s.run(&["provision", &wt]);
    provision.rc(0);
    provision.out_has("config");
    assert!(
        !exists(format!("{outside}/app.local")),
        "seeding wrote through a symlink"
    );
    assert_eq!(
        read(format!("{wt}/.env")),
        "secret",
        "one refused file stopped the rest"
    );
}

// Only regular files: a symlink in the main checkout could point at anything.
#[test]
fn seed_skips_a_symlink_source() {
    let s = seeded_project();
    let main = &s.main;
    rm(format!("{main}/.env"));
    std::os::unix::fs::symlink("/etc/hostname", format!("{main}/.env"))
        .expect("the symlink should be made");
    let wt = add(&s, "seeded");
    s.run(&["provision", &wt]).rc(0);
    assert!(
        !exists(format!("{wt}/.env")) && !is_symlink(format!("{wt}/.env")),
        "a symlink source was copied"
    );
}

#[test]
fn seed_failure_warns_and_exits_zero() {
    let s = seeded_project();
    let main = &s.main;
    let root = running_as_root(&s);
    let wt = add(&s, "seeded");
    chmod(format!("{main}/.env"), 0o000);
    let provision = s.run(&["provision", &wt]);
    chmod(format!("{main}/.env"), 0o600);
    provision.rc(0);
    if !root {
        provision.out_has(".env");
        assert!(!exists(format!("{wt}/.env")), "left a partial copy behind");
    }
    assert_eq!(
        read(format!("{wt}/config/app.local")),
        "local",
        "one failure stopped the rest"
    );

    // The target cannot be written: not the same as already existing, so it has to warn.
    let read_only = add(&s, "seeded-ro");
    rm(format!("{read_only}/.gitmodules"));
    rm_rf(format!("{read_only}/sub"));
    chmod(&read_only, 0o555);
    let provision = s.run(&["provision", &read_only]);
    chmod(&read_only, 0o755);
    provision.rc(0);
    if !root {
        provision.out_has("not copying .env");
    }

    // The whole step failing: git cannot list the files. A corrupt index in the main
    // checkout is real, so it works on a wrapped binary too, whose own git comes ahead of
    // anything put on PATH.
    let listing_fails = add(&s, "seeded-b");
    write(format!("{main}/.git/index"), "garbage\n");
    let provision = s.run(&["provision", &listing_fails]);
    provision.rc(0);
    provision.out_has("seed step FAILED");
}

// Provisioning the main checkout has nothing to copy from, and says nothing.
#[test]
fn seed_skips_the_main_checkout() {
    let s = seeded_project();
    let provision = s.run(&["provision", &s.main]);
    provision.rc(0);
    assert!(
        !provision.out.contains("seed") && !provision.err.contains("seed"),
        "seeded the main checkout: {} {}",
        provision.out,
        provision.err
    );
}
