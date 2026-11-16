//! `move`: a worktree goes where placement says it belongs, or where it is told, with
//! everything in it — submodules, uncommitted work, local commits — exactly as it was.

use crate::support::{
    Sandbox, Worktree, append, exists, is_dir, is_file, mkdir, read, rm_rf, write,
};

/// Whether git has `path` registered as a worktree of the repository at `main`.
fn registered(s: &Sandbox, main: &str, path: &str) -> bool {
    s.git(main, &["worktree", "list", "--porcelain"])
        .lines()
        .any(|line| line == format!("worktree {path}"))
}

/// A repository with no submodules at t/plain, and a worktree of it far from where
/// placement puts it, holding an uncommitted edit and an untracked file.
fn plain_project(s: &Sandbox) -> (String, String) {
    let main = format!("{}/plain", s.t);
    mkdir(&main);
    s.git(&main, &["init", "-q"]);
    s.commit_file(&main, "README", "plain");
    let wt = format!("{}/elsewhere/plain", s.t);
    s.git(&main, &["worktree", "add", "-q", "-b", "plain", &wt]);
    append(format!("{wt}/README"), "edited\n");
    write(format!("{wt}/untracked"), "scratch\n");
    (main, wt)
}

/// The `kind` of the first reason in a `check --json` document.
#[track_caller]
fn kind_of(document: &str) -> String {
    document
        .lines()
        .find_map(|line| line.trim().strip_prefix("\"kind\": "))
        .unwrap_or_else(|| panic!("no kind in {document}"))
        .trim_end_matches(',')
        .to_owned()
}

#[track_caller]
fn assert_intact_plain(s: &Sandbox, main: &str, old: &str, new: &str) {
    assert!(!exists(old), "{old} is still there");
    assert!(registered(s, main, new), "git does not list {new}");
    assert!(!registered(s, main, old), "git still lists {old}");
    assert_eq!(s.git(new, &["branch", "--show-current"]).trim(), "plain");
    assert_eq!(read(format!("{new}/README")), "plain\nedited");
    assert_eq!(read(format!("{new}/untracked")), "scratch");
}

// AC1: with no destination, a worktree goes beside its repository, and the directory that
// holds worktrees there is made when it is missing.
#[test]
fn move_places_a_worktree_without_submodules_beside_its_repository() {
    let s = Sandbox::new();
    let (main, old) = plain_project(&s);
    let new = format!("{}/plain.worktrees/plain", s.t);
    let moved = s.run(&["move", &old]);
    moved.rc(0);
    moved.out_has(&old);
    moved.out_has(&new);
    assert_intact_plain(&s, &main, &old, &new);
}

// AC2: a destination given is where it goes.
#[test]
fn move_takes_an_explicit_destination() {
    let s = Sandbox::new();
    let (main, old) = plain_project(&s);
    let new = format!("{}/chosen/place", s.t);
    s.run(&["move", &old, &new]).rc(0);
    assert_intact_plain(&s, &main, &old, &new);
}

// AC3: git refuses to move a worktree holding submodules. This one has a nested submodule,
// an unpushed commit in the submodule, untracked work in it and an edit in the worktree,
// and all of it arrives working, with every pointer relative as git writes them.
#[test]
fn move_carries_submodules_and_nested_submodules() {
    let s = Sandbox::with_project();
    let wt = s.worktree("carried");
    s.commit_in_submodule(&wt);
    write(wt.file("sub/untracked"), "scratch\n");
    append(wt.file("README"), "edited\n");
    // The same verdict before and after, whichever of its refusals check reports first.
    let before = s.run(&["check", "--json", &wt.path]);
    before.rc(1);
    let refused_as = kind_of(&before.out);
    let new = format!("{}/project.worktrees/wt-carried", s.t);
    let moved_wt = Worktree {
        path: new.clone(),
        gitdir: wt.gitdir.clone(),
    };

    s.run(&["move", &wt.path]).rc(0);

    assert!(!exists(&wt.path), "the old directory is still there");
    assert!(registered(&s, &s.main, &new), "git does not list {new}");
    for (dir, clone) in [
        ("sub", "modules/sub"),
        ("sub/nested", "modules/sub/modules/nested"),
    ] {
        let dir = moved_wt.file(dir);
        assert_eq!(
            s.git(&dir, &["rev-parse", "--absolute-git-dir"]).trim(),
            wt.git_file(clone),
            "{dir} does not find its own clone"
        );
        assert_eq!(s.git(&dir, &["rev-parse", "--show-toplevel"]).trim(), dir);
        s.git(&dir, &["status", "--porcelain"]);
        assert!(
            !read(format!("{dir}/.git")).starts_with("gitdir: /"),
            "{dir}/.git is not relative"
        );
        let config = format!("{}/config", wt.git_file(clone));
        let core_worktree = s.git(&s.root, &["config", "--file", &config, "core.worktree"]);
        assert!(
            !core_worktree.starts_with('/'),
            "core.worktree in {config} is not relative"
        );
    }
    assert_eq!(
        s.git(&moved_wt.file("sub"), &["log", "-1", "--format=%s"])
            .trim(),
        "unpushed in sub"
    );
    assert_eq!(read(moved_wt.file("sub/untracked")), "scratch");
    assert!(read(moved_wt.file("README")).ends_with("edited"));
    let after = s.run(&["check", "--json", &new]);
    after.rc(1);
    assert_eq!(kind_of(&after.out), refused_as);
}

// Moving the last worktree out of its place tidies the emptied directory, as remove does.
#[test]
fn move_out_of_its_place_tidies_the_empty_directory() {
    let s = Sandbox::with_project();
    let placed = s.create("leaving");
    let dir = format!("{}/project.worktrees", s.t);
    let new = format!("{}/elsewhere/leaving", s.t);
    s.run(&["move", &placed, &new]).rc(0);
    assert!(registered(&s, &s.main, &new));
    assert!(!exists(&dir), "{dir} was left behind empty");
}

// AC4: a worktree already where placement puts it stays, and that is a success.
#[test]
fn move_of_a_placed_worktree_changes_nothing() {
    let s = Sandbox::with_project();
    let placed = s.create("placed");
    let moved = s.run(&["move", &placed]);
    moved.rc(0);
    moved.out_has("already");
    assert!(
        is_file(format!("{placed}/.git")),
        "the worktree went somewhere"
    );
    assert!(registered(&s, &s.main, &placed));
}

/// What a refused move must leave: the worktree where it was, working, and nothing at
/// the target.
#[track_caller]
fn unchanged(s: &Sandbox, wt: &Worktree, target: &str) {
    assert!(is_dir(&wt.path), "the worktree left {}", wt.path);
    assert!(
        registered(s, &s.main, &wt.path),
        "git no longer lists {}",
        wt.path
    );
    assert!(
        s.git_ok(&wt.file("sub"), &["status", "--porcelain"]),
        "the submodule stopped working"
    );
    assert!(
        !exists(format!("{target}/.git")),
        "something arrived at {target}"
    );
}

// AC5: each refusal comes before anything changes.
#[test]
fn move_refuses_with_nothing_changed() {
    let s = Sandbox::with_project();
    let wt = s.worktree("stays");
    let target = format!("{}/project.worktrees/wt-stays", s.t);

    {
        let holder = s.holder_in(&wt.path);
        let refused = s.run(&["move", &wt.path]);
        refused.rc(1);
        refused.err_has("in use");
        refused.err_has(&format!("pid {}", holder.pid()));
    }
    unchanged(&s, &wt, &target);

    s.run(&["lock", "--reason", "agent-7", &wt.path]).rc(0);
    let refused = s.run(&["move", &wt.path]);
    refused.rc(1);
    refused.err_has("locked");
    refused.err_has("agent-7");
    unchanged(&s, &wt, &target);
    s.run(&["unlock", &wt.path]).rc(0);

    mkdir(&target);
    let refused = s.run(&["move", &wt.path]);
    refused.rc(1);
    refused.err_has(&target);
    unchanged(&s, &wt, &target);
    rm_rf(&target);

    let refused = s.run(&["move", &s.main]);
    refused.rc(1);
    refused.err_has("main worktree");
    unchanged(&s, &wt, &target);
}

// AC6: a worktree that cannot be snapshotted is not moved: a stranded submodule, whose
// clone is gone, cannot be read before, so nothing could be proven after.
#[test]
fn move_cannot_snapshot_a_stranded_submodule_and_changes_nothing() {
    let s = Sandbox::with_project();
    let wt = s.worktree("stranded");
    rm_rf(wt.git_file("modules"));
    let refused = s.run(&["move", &wt.path]);
    refused.rc(3);
    assert!(is_dir(&wt.path), "the worktree moved");
    assert!(registered(&s, &s.main, &wt.path));
}
