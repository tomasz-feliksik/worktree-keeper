//! `guard` and `remove`: what may be deleted, and what a refusal leaves alone.

use crate::support::{Sandbox, Worktree, append, exists, is_dir, is_file, ran, rm, rm_rf, write};

// The silent-128 reproduction (2026-09-22): after a deinit, the NESTED submodule's gitdir
// keeps core.worktree pointing at a directory that no longer exists. Removal must succeed,
// and must never exit silently.
#[test]
fn nested_stale_core_worktree() {
    let s = Sandbox::with_project();
    let wt = s.worktree("stale");
    s.git(&wt.path, &["submodule", "-q", "deinit", "-f", "--all"]);
    let nested_gitdir = wt.git_file("modules/sub/modules/nested");
    assert!(
        is_dir(&nested_gitdir),
        "setup: no nested gitdir at {nested_gitdir}"
    );
    assert!(
        !s.git_ok(
            &s.root,
            &[&format!("--git-dir={nested_gitdir}"), "log", "-1"]
        ),
        "setup: the nested gitdir is readable without --work-tree, so not the failing shape"
    );
    s.run(&["remove", &wt.path]).rc(0);
    assert!(!exists(&wt.path), "worktree directory still exists");
}

// An unpushed commit in a POPULATED submodule is refused, and named.
#[test]
fn unpushed_populated_submodule() {
    let s = Sandbox::with_project();
    let wt = s.worktree("pop");
    s.commit_in_submodule(&wt);
    let guard = s.run(&["guard", &wt.path]);
    guard.rc(1);
    guard.err_has("holds commits no remote has");
    guard.err_has("modules/sub");
    assert!(
        exists(wt.file("sub/.git")),
        "guard deinitialised a submodule it refused to clear"
    );
}

// --force covers only the worktree's own uncommitted changes: an unpushed submodule commit
// is refused under it, on remove and on guard alike.
#[test]
fn unpushed_refused_under_force() {
    let s = Sandbox::with_project();
    let wt = s.worktree("forced");
    s.commit_in_submodule(&wt);
    let guard = s.run(&["guard", "--force", &wt.path]);
    guard.rc(1);
    guard.err_has("holds commits no remote has");
    let remove = s.run(&["remove", "--force", &wt.path]);
    remove.rc(1);
    remove.err_has("holds commits no remote has");
    assert!(
        exists(wt.file("sub/README")),
        "--force emptied a submodule holding an unpushed commit"
    );
}

// Removing one worktree must not cost ANOTHER worktree's clones. `git worktree prune` works
// on the whole repository, so a different registration whose directory is gone would lose
// an unpushed submodule clone that sweep refuses to touch.
#[test]
fn remove_keeps_other_stale_registrations() {
    let s = Sandbox::with_project();
    let stale = s.worktree("stale-other");
    s.commit_in_submodule(&stale);
    rm_rf(&stale.path);
    let bystander = s.worktree("bystander");
    s.run(&["remove", &bystander.path]).rc(0);
    assert!(
        is_dir(stale.git_file("modules/sub")),
        "removing another worktree pruned the unpushed clone"
    );
}

// An unpushed commit on a DETACHED head, the normal state of a submodule and on no branch
// at all, is refused.
#[test]
fn unpushed_detached_submodule() {
    let s = Sandbox::with_project();
    let wt = s.worktree("det");
    append(wt.file("sub/README"), "change\n");
    s.git(
        &wt.file("sub"),
        &["commit", "-q", "-am", "unpushed, detached"],
    );
    let guard = s.run(&["guard", &wt.path]);
    guard.rc(1);
    guard.err_has("holds commits no remote has");
}

// An unpushed commit in a DEINITIALISED submodule's leftover clone is refused.
#[test]
fn unpushed_leftover_gitdir() {
    let s = Sandbox::with_project();
    let wt = s.worktree("left");
    s.commit_in_submodule(&wt);
    s.git(&wt.path, &["submodule", "-q", "deinit", "-f", "--all"]);
    assert!(!exists(wt.file("sub/.git")), "setup: sub still populated");
    let guard = s.run(&["guard", &wt.path]);
    guard.rc(1);
    guard.err_has("holds commits no remote has");
    assert!(
        is_dir(wt.git_file("modules")),
        "guard cleared modules/ it had refused"
    );
}

// Uncommitted changes in the worktree itself are refused; --force accepts them.
#[test]
fn dirty_worktree() {
    let s = Sandbox::with_project();
    let wt = s.worktree("dirty");
    write(wt.file("untracked-file"), "scratch\n");
    let refused = s.run(&["remove", &wt.path]);
    refused.rc(1);
    refused.err_has("uncommitted changes");
    assert!(
        exists(&wt.path),
        "refused removal still deleted the worktree"
    );
    s.run(&["remove", "--force", &wt.path]).rc(0);
    assert!(
        !exists(&wt.path),
        "--force did not remove the dirty worktree"
    );
}

// Uncommitted changes in a SUBMODULE are refused even with --force.
#[test]
fn dirty_submodule_refused_under_force() {
    let s = Sandbox::with_project();
    let wt = s.worktree("dsub");
    append(wt.file("sub/README"), "change\n");
    let refused = s.run(&["remove", "--force", &wt.path]);
    refused.rc(1);
    refused.err_has("submodule sub");
    assert!(
        exists(&wt.path),
        "worktree deleted despite a dirty submodule"
    );
}

// A probe that cannot run stops with status 3 and names the path, never a silent exit.
// Shape: a submodule clone whose HEAD is garbage.
#[test]
fn probe_failure_is_loud() {
    let s = Sandbox::with_project();
    let wt = s.worktree("probe");
    s.git(&wt.path, &["submodule", "-q", "deinit", "-f", "--all"]);
    write(wt.git_file("modules/sub/HEAD"), "garbage\n");
    let guard = s.run(&["guard", &wt.path]);
    guard.rc(3);
    guard.err_has("cannot inspect");
    guard.err_has("modules/sub");
}

#[test]
fn main_worktree_refused() {
    let s = Sandbox::with_project();
    let guard = s.run(&["guard", &s.main]);
    guard.rc(1);
    guard.err_has("main worktree");
}

// guard never removes: after it passes, the worktree is still there and still registered,
// and only its submodule metadata is gone.
#[test]
fn guard_never_removes() {
    let s = Sandbox::with_project();
    let wt = s.worktree("keep");
    let guard = s.run(&["guard", &wt.path]);
    guard.rc(0);
    guard.out_has("emptied 1 submodule");
    assert!(is_dir(&wt.path), "guard removed the worktree directory");
    let listed = s.git(&s.main, &["worktree", "list", "--porcelain"]);
    let registered = format!("worktree {}", wt.path);
    assert!(
        listed.lines().any(|line| line == registered),
        "guard unregistered the worktree"
    );
    assert!(
        !is_dir(wt.git_file("modules")),
        "guard left <gitdir>/modules behind"
    );
    assert!(
        s.git_ok(&s.main, &["worktree", "remove", &wt.path]),
        "plain 'git worktree remove' still refuses after guard"
    );
}

// remove deletes a merged branch, refuses to delete an unmerged one.
#[test]
fn remove_branch_disposition() {
    let s = Sandbox::with_project();
    let merged = s.worktree("merged");
    s.run(&["remove", &merged.path]).rc(0);
    assert!(
        !s.has_branch(&s.main, "merged"),
        "merged branch was not deleted"
    );
    let unmerged = s.worktree("unmerged");
    append(unmerged.file("README"), "change\n");
    s.git(&unmerged.path, &["commit", "-q", "-am", "unmerged work"]);
    let refused = s.run(&["remove", &unmerged.path]);
    refused.rc(1);
    refused.err_has("not merged");
    assert!(
        s.has_branch(&s.main, "unmerged"),
        "unmerged branch was deleted without --force"
    );
}

fn commit_change(s: &Sandbox, wt: &Worktree, message: &str) {
    append(wt.file("README"), "change\n");
    s.git(&wt.path, &["commit", "-q", "-am", message]);
}

// "Refused" means nothing happened. The unmerged branch is found before anything is deleted:
// the worktree and its submodule stay. --keep-branch and --force skip it.
#[test]
fn unmerged_refusal_changes_nothing() {
    let s = Sandbox::with_project();
    let half = s.worktree("half");
    commit_change(&s, &half, "unmerged work");
    let refused = s.run(&["remove", &half.path]);
    refused.rc(1);
    refused.err_has("not merged");
    assert!(
        is_file(half.file("README")),
        "the worktree was removed before refusing"
    );
    assert!(
        exists(half.file("sub/README")),
        "the submodule was emptied before refusing"
    );
    assert!(
        is_dir(half.git_file("modules")),
        "the submodule clones were cleared before refusing"
    );
    assert_eq!(
        s.git(&half.path, &["branch", "--show-current"]),
        "half",
        "the worktree was detached before refusing"
    );

    // git's own rule: merged into the upstream counts, although HEAD lacks it.
    s.git(&half.path, &["push", "-q", "-u", "origin", "half"]);
    s.run(&["remove", "--keep-branch", &half.path]).rc(0);
    assert!(
        s.has_branch(&s.main, "half"),
        "--keep-branch deleted the branch"
    );

    let pushed = s.worktree("pushed");
    commit_change(&s, &pushed, "pushed, not merged locally");
    s.git(&pushed.path, &["push", "-q", "-u", "origin", "pushed"]);
    s.run(&["remove", &pushed.path]).rc(0);
    assert!(
        !exists(&pushed.path),
        "a branch merged into its upstream was refused"
    );

    let discarded = s.worktree("discarded");
    commit_change(&s, &discarded, "to discard");
    s.run(&["remove", "--force", &discarded.path]).rc(0);
    assert!(
        !s.has_branch(&s.main, "discarded"),
        "--force kept the branch"
    );
}

// The remote branch goes last and only on request: kept when the local deletion refuses,
// kept without --delete-remote, deleted with it, from the branch's own remote rather than
// always from origin.
#[test]
fn remote_branch_deleted_last() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    let bare = format!("{t}/project.git");

    let unmerged = s.worktree("unmerged");
    commit_change(&s, &unmerged, "unmerged");
    s.git(&unmerged.path, &["push", "-q", "-u", "origin", "unmerged"]);
    // Merged to nothing: `branch -d` checks HEAD and the upstream, so take the upstream away.
    s.git(&unmerged.path, &["branch", "--unset-upstream"]);
    s.run(&["remove", "--delete-remote", &unmerged.path]).rc(1);
    assert!(
        s.has_branch(&bare, "unmerged"),
        "the remote branch went although the local deletion refused"
    );

    // Past the unmerged check, so that only the order protects the remote: a lock on the
    // ref makes the local deletion itself fail.
    let locked = s.worktree("locked-ref");
    s.git(&locked.path, &["push", "-q", "-u", "origin", "locked-ref"]);
    let lock = format!("{main}/.git/refs/heads/locked-ref.lock");
    write(&lock, "");
    let failed = s.run(&["remove", "--delete-remote", &locked.path]);
    rm(&lock);
    failed.rc(1);
    assert!(
        s.has_branch(&bare, "locked-ref"),
        "the remote branch went before the local deletion failed"
    );

    let plain = s.worktree("plain");
    s.git(&plain.path, &["push", "-q", "-u", "origin", "plain"]);
    s.run(&["remove", &plain.path]).rc(0);
    assert!(
        s.has_branch(&bare, "plain"),
        "the remote branch was deleted without --delete-remote"
    );

    let asked = s.worktree("asked");
    s.git(&asked.path, &["push", "-q", "-u", "origin", "asked"]);
    s.run(&["remove", "--delete-remote", &asked.path]).rc(0);
    assert!(
        !s.has_branch(&bare, "asked"),
        "--delete-remote did not delete the remote branch"
    );
    assert!(!s.has_branch(main, "asked"), "the local branch survived");

    let absent = s.worktree("absent");
    let removed = s.run(&["remove", "--delete-remote", &absent.path]);
    removed.rc(0);
    removed.out_has("has no branch absent");

    s.git(main, &["remote", "add", "other", &bare]);
    let elsewhere = s.worktree("elsewhere");
    s.git(&elsewhere.path, &["push", "-q", "-u", "other", "elsewhere"]);
    s.git(main, &["remote", "rename", "origin", "old-origin"]);
    s.run(&["remove", "--delete-remote", &elsewhere.path]).rc(0);
    assert!(
        !s.has_branch(&bare, "elsewhere"),
        "the branch's own remote was not the one asked"
    );
}

// A remote that cannot answer is not a remote with no such branch.
#[test]
fn unreachable_remote_is_could_not_tell() {
    let s = Sandbox::with_project();
    let t = &s.t;
    let wt = s.worktree("away");
    s.git(&wt.path, &["push", "-q", "-u", "origin", "away"]);
    s.git(
        &s.main,
        &["remote", "set-url", "origin", &format!("{t}/nowhere.git")],
    );
    let removed = s.run(&["remove", "--delete-remote", &wt.path]);
    removed.rc(3);
    removed.err_has("cannot inspect origin");
    assert!(
        s.has_branch(&format!("{t}/project.git"), "away"),
        "the branch left the remote"
    );
}

// A top-level submodule in a SUBDIRECTORY is still top-level. The guard used to call a
// submodule nested whenever its path had a slash, so it deinitialised none of
// `frontend/design-system`, `lib/shared`, then cleared modules/ under them, and the worktree
// could never be removed (2026-09-27).
#[test]
fn top_level_submodule_in_subdirectory() {
    let s = Sandbox::with_project_at("libs/sub");
    let wt = s.worktree("subdir");
    assert!(
        exists(wt.file("libs/sub/nested/.git")),
        "setup: the nested submodule is not populated"
    );
    let removed = s.run(&["remove", &wt.path]);
    removed.rc(0);
    assert!(
        !exists(&wt.path),
        "worktree still exists; stderr: {}",
        removed.err
    );
}

fn submodule_config(s: &Sandbox) -> Vec<String> {
    let mut lines: Vec<String> = s
        .git(&s.main, &["config", "--get-regexp", r"^submodule\."])
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

// Removing a worktree must not touch the MAIN checkout. Submodule registration
// (submodule.<name>.url/active/update) lives in the config file every worktree shares, so
// `git submodule deinit` run in a linked worktree de-registers the submodules for the main
// checkout too. Measured 2026-09-27, where it also dropped a local `update = rebase`.
#[test]
fn remove_keeps_main_submodule_config() {
    let s = Sandbox::with_project_at("libs/sub");
    s.git(&s.main, &["config", "submodule.libs/sub.update", "rebase"]);
    let before = submodule_config(&s);
    assert!(
        before
            .iter()
            .any(|line| line.contains("submodule.libs/sub.url")),
        "setup: main checkout has no submodule registration"
    );
    let wt = s.worktree("cfg");
    s.run(&["remove", &wt.path]).rc(0);
    assert_eq!(
        submodule_config(&s),
        before,
        "main checkout's submodule config changed"
    );
}

// The shape an old guard left behind: modules/ cleared while the submodules stayed
// populated, each .git file pointing at a gitdir that is gone.
fn strand(s: &Sandbox, wt: &Worktree) {
    rm_rf(wt.git_file("modules"));
    assert!(
        !s.git_ok(&wt.file("sub"), &["rev-parse", "--git-dir"]),
        "setup: the submodule still resolves its gitdir, so not stranded"
    );
}

// A stranded worktree whose submodule files match their recorded commits is removed: nothing
// in it exists nowhere else, and the main checkout's clone of each submodule proves it.
#[test]
fn stranded_worktree_recovered() {
    let s = Sandbox::with_project();
    let wt = s.worktree("stranded");
    strand(&s, &wt);
    let removed = s.run(&["remove", &wt.path]);
    removed.rc(0);
    assert!(
        !exists(&wt.path),
        "stranded worktree was not removed: {}",
        removed.err
    );
}

// ...and one whose submodule files DIFFER from the recorded commit is refused, naming the
// file, at any depth, with nothing deleted.
#[test]
fn stranded_modified_refused() {
    let s = Sandbox::with_project();
    let wt = s.worktree("strandmod");
    append(wt.file("sub/nested/README"), "only here\n");
    strand(&s, &wt);
    let refused = s.run(&["remove", &wt.path]);
    refused.rc(1);
    refused.err_has("sub/nested");
    refused.err_has("README");
    assert!(
        exists(wt.file("sub/nested/README")),
        "refused removal still deleted the modified file"
    );
}

// The shape main leaves behind when it RETIRES a submodule: main no longer declares it and
// its clone under <common>/modules is gone, while the worktree still has the submodule and
// its own clone under <gitdir>/modules. The gitfile is made stale the way a repository move
// makes it, a relative path that no longer resolves, so the guard can reach the clone only
// by --git-dir.
fn retire_submodule(s: &Sandbox, wt: &Worktree) {
    let main = &s.main;
    s.git(main, &["rm", "-q", "sub"]);
    s.git(main, &["rm", "-qf", "--ignore-unmatch", ".gitmodules"]);
    s.git(main, &["commit", "-q", "-m", "retire sub"]);
    // `git rm` may have taken the section with it already, so its absence is fine.
    s.git_ok(main, &["config", "--remove-section", "submodule.sub"]);
    rm_rf(format!("{main}/.git/modules/sub"));
    let name = wt
        .path
        .rsplit('/')
        .next()
        .expect("a path has a last component");
    write(
        wt.file("sub/.git"),
        &format!("gitdir: ../../moved-away/.git/worktrees/{name}/modules/sub\n"),
    );
    assert!(
        is_dir(wt.git_file("modules/sub")),
        "setup: the worktree's own clone is gone"
    );
    assert!(
        !exists(format!("{main}/.gitmodules")),
        "setup: main still declares the submodule"
    );
}

// A retired submodule whose files match their recorded commit is verified against the
// worktree's OWN clone, and the worktree is removed.
#[test]
fn stranded_retired_submodule_recovered() {
    let s = Sandbox::with_project();
    let wt = s.worktree("retired");
    retire_submodule(&s, &wt);
    let removed = s.run(&["remove", "--force", &wt.path]);
    removed.rc(0);
    assert!(
        !exists(&wt.path),
        "worktree with a retired, clean submodule was not removed: {}",
        removed.err
    );
}

// ...and one with a modified or untracked file is still refused, naming both.
#[test]
fn stranded_retired_modified_refused() {
    let s = Sandbox::with_project();
    let wt = s.worktree("retmod");
    retire_submodule(&s, &wt);
    append(wt.file("sub/README"), "only here\n");
    write(wt.file("sub/untracked-note"), "new\n");
    let refused = s.run(&["remove", "--force", &wt.path]);
    refused.rc(1);
    refused.err_has("README");
    refused.err_has("untracked-note");
    assert!(
        exists(wt.file("sub/untracked-note")),
        "refused removal still deleted the untracked file"
    );
}

// With no clone anywhere, the advice must work although main no longer declares the
// submodule: `submodule update --init` there is a no-op.
#[test]
fn stranded_retired_no_clone() {
    let s = Sandbox::with_project();
    let t = &s.t;
    let wt = s.worktree("retnone");
    retire_submodule(&s, &wt);
    rm_rf(wt.git_file("modules/sub"));
    let mut removed = s.run(&["remove", "--force", &wt.path]);
    removed.rc(3);
    assert!(
        !removed.err.contains("submodule update --init"),
        "advice names 'submodule update --init', which main cannot do: {}",
        removed.err
    );
    removed.err_has(&format!("git clone --bare {t}/sub.git"));
    assert!(
        exists(wt.file("sub/README")),
        "unverifiable submodule was emptied"
    );

    // And the advice works: run the command it names, once per submodule level, the nested
    // one included, and removal goes through.
    for _ in 0..3 {
        let advised: Vec<String> = removed
            .err
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("git clone --bare "))
            .map(str::to_owned)
            .collect();
        if advised.is_empty() {
            break;
        }
        for command in advised {
            let followed = ran(s.command("bash").args(["-c", &format!("{command} -q")]));
            followed.rc(0);
        }
        removed = s.run(&["remove", "--force", &wt.path]);
        if removed.rc != 3 {
            break;
        }
    }
    removed.rc(0);
    assert!(
        !exists(&wt.path),
        "worktree not removed after following the advice: {}",
        removed.err
    );
}

// workmux runs pre_remove in the worktree, so no path means the current directory. It also
// sets WM_WORKTREE_PATH, which this tool once read: naming another worktree there proves it
// no longer does.
#[test]
fn guard_defaults_to_the_current_directory() {
    let s = Sandbox::with_project();
    let here = s.worktree("here");
    let other = s.worktree("other");
    let guard = ran(s
        .tool()
        .current_dir(&here.path)
        .env("WM_WORKTREE_PATH", &other.path)
        .arg("guard"));
    guard.rc(0);
    guard.out_has(&format!("{} may be removed", here.path));
    assert!(
        !guard.out.contains(&other.path),
        "guard acted on WM_WORKTREE_PATH: {}",
        guard.out
    );
    assert!(
        is_dir(other.git_file("modules")),
        "guard cleared the worktree the variable named"
    );
}
