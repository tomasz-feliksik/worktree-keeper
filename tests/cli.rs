//! The exit contract, through the real binary.

use std::process::{Command, Output};

fn worktree(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_worktree"))
        .args(args)
        .output()
        .expect("the binary under test should start")
}

#[test]
fn help_exits_zero_and_lists_every_command() {
    let output = worktree(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8_lossy(&output.stdout);
    for command in ["create", "provision", "actor", "guard", "remove", "sweep"] {
        assert!(
            help.contains(command),
            "--help should list {command}: {help}"
        );
    }
}

#[test]
fn a_usage_error_exits_two() {
    assert_eq!(worktree(&["no-such-command"]).status.code(), Some(2));
    assert_eq!(worktree(&["remove"]).status.code(), Some(2));
    assert_eq!(
        worktree(&["create", "branch", "--bogus"]).status.code(),
        Some(2)
    );
}

#[test]
fn an_empty_placement_root_is_refused_as_no_placement_root() {
    for (command, args) in [
        ("sweep", vec!["sweep"]),
        ("create", vec!["create", "some-branch"]),
    ] {
        let unset = Command::new(env!("CARGO_BIN_EXE_worktree"))
            .args(&args)
            .env_remove("WORKTREE_ROOT")
            .output()
            .unwrap();
        let empty = Command::new(env!("CARGO_BIN_EXE_worktree"))
            .args(&args)
            .env("WORKTREE_ROOT", "")
            .output()
            .unwrap();
        for output in [unset, empty] {
            assert_eq!(output.status.code(), Some(1), "{command}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("WORKTREE_ROOT"), "{command}: {stderr}");
        }
    }
}

#[test]
fn a_directory_outside_git_is_could_not_tell_rather_than_safe() {
    let output = worktree(&["guard", "/"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot inspect"));
}

#[test]
fn a_path_that_is_not_a_directory_is_refused() {
    let output = worktree(&["guard", "/nonexistent"]);
    assert_eq!(output.status.code(), Some(1));
}
