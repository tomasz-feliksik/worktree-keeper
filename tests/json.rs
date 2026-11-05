//! The `--json` contract in docs/json-schema.md, through the real binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("wk-json-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(dir.join("main")).unwrap();
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        fs::write(repo.main().join("README"), "x").unwrap();
        repo.git(&["add", "README"]);
        repo.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "init",
        ]);
        repo.git(&["worktree", "add", "-q", "-b", "work", "../wt"]);
        repo
    }

    fn main(&self) -> PathBuf {
        self.dir.join("main")
    }

    fn worktree(&self) -> PathBuf {
        self.dir.join("wt")
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(self.main())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A submodule clone whose state cannot be read, so the unpushed check cannot run.
    fn break_a_clone(&self) {
        let clone = self.main().join(".git/worktrees/wt/modules/sub");
        fs::create_dir_all(clone.join("objects")).unwrap();
        fs::create_dir_all(clone.join("refs")).unwrap();
        fs::write(clone.join("HEAD"), "garbage\n").unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.dir));
    }
}

fn worktree(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_worktree"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON document ({error}): {output:?}"))
}

#[test]
fn check_says_removable_with_no_reasons_and_exits_zero() {
    let repo = Repo::new("removable");
    let output = worktree(&["check", "--json"], &repo.worktree());
    let json = document(&output);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["verdict"], "removable");
    assert_eq!(json["reasons"], serde_json::json!([]));
    assert!(json["path"].as_str().unwrap().ends_with("/wt"));
}

#[test]
fn check_names_the_kind_of_a_refusal_and_exits_one() {
    let repo = Repo::new("refused");
    fs::write(repo.worktree().join("scratch"), "x").unwrap();
    let output = worktree(&["check", "--json"], &repo.worktree());
    let json = document(&output);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json["verdict"], "refused");
    assert_eq!(json["reasons"][0]["kind"], "dirty");
    assert!(
        json["reasons"][0]["message"]
            .as_str()
            .unwrap()
            .contains("uncommitted")
    );
}

#[test]
fn check_calls_the_main_checkout_what_it_is() {
    let repo = Repo::new("main");
    let output = worktree(&["check", "--json"], &repo.main());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(document(&output)["reasons"][0]["kind"], "main_worktree");
}

#[test]
fn check_that_could_not_look_says_unknown_and_exits_three() {
    let repo = Repo::new("unknown");
    repo.break_a_clone();
    let output = worktree(&["check", "--json"], &repo.worktree());
    let json = document(&output);
    assert_eq!(output.status.code(), Some(3), "{json}");
    assert_eq!(json["verdict"], "unknown");
    assert_eq!(json["reasons"][0]["kind"], "could_not_check");
}

#[test]
fn check_force_accepts_a_dirty_worktree_as_guard_does() {
    let repo = Repo::new("force");
    fs::write(repo.worktree().join("scratch"), "x").unwrap();
    let output = worktree(&["check", "--json", "--force"], &repo.worktree());
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(document(&output)["verdict"], "removable");
}

#[test]
fn status_reports_every_field_of_a_worktree() {
    let repo = Repo::new("status");
    let output = worktree(&["status", "--json"], &repo.worktree());
    let json = document(&output);
    assert_eq!(output.status.code(), Some(0), "{json}");
    assert_eq!(json["schema_version"], 1);
    let row = &json["worktrees"][0];
    assert!(row["path"].as_str().unwrap().ends_with("/wt"));
    assert!(row["repo"].as_str().unwrap().ends_with("/main"));
    assert_eq!(row["branch"], "work");
    assert_eq!(row["dirty"], false);
    assert_eq!(row["unpushed"], false);
    assert_eq!(row["merged"], true);
    assert_eq!(row["locked"], Value::Null);
    assert_eq!(row["in_use"], serde_json::json!([]));
    assert_eq!(row["verdict"], "removable");
}

#[test]
fn status_counts_an_unmerged_branch_as_a_refusal_but_still_exits_zero() {
    let repo = Repo::new("unmerged");
    let wt = repo.worktree();
    fs::write(wt.join("new"), "x").unwrap();
    for args in [
        vec!["add", "new"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "work",
        ],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&wt)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let output = worktree(&["status", "--json"], &wt);
    let json = document(&output);
    assert_eq!(output.status.code(), Some(0));
    let row = &json["worktrees"][0];
    assert_eq!(row["merged"], false);
    assert_eq!(row["verdict"], "refused");
    assert_eq!(row["reasons"][0]["kind"], "unmerged_branch");
}

#[test]
fn status_that_could_not_read_a_worktree_is_unknown_never_safe_and_exits_three() {
    let repo = Repo::new("status-unknown");
    repo.break_a_clone();
    let output = worktree(&["status", "--json"], &repo.worktree());
    let json = document(&output);
    assert_eq!(output.status.code(), Some(3), "{json}");
    let row = &json["worktrees"][0];
    assert_eq!(row["verdict"], "unknown");
    assert_eq!(row["unpushed"], Value::Null, "unread is null, never false");
    assert_eq!(row["reasons"][0]["kind"], "could_not_check");
}

#[test]
fn status_reports_a_lock_and_its_reason() {
    let repo = Repo::new("locked");
    let lock = worktree(&["lock", "--reason", "agent-7"], &repo.worktree());
    assert_eq!(lock.status.code(), Some(0));
    let json = document(&worktree(&["status", "--json"], &repo.worktree()));
    let row = &json["worktrees"][0];
    assert_eq!(row["locked"], "agent-7");
    assert_eq!(row["reasons"][0]["kind"], "locked");
}
