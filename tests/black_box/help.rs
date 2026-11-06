use crate::support::{Sandbox, exists};

// --help on every subcommand prints usage and acts on nothing, and a mistyped option is
// refused as usage. `create --help` used to take `--help` as the branch name; a leading
// dash is never read as a branch or a path.
#[test]
fn help_never_acts() {
    let s = Sandbox::with_project();
    let (t, main) = (&s.t, &s.main);
    for sub in [
        "status",
        "check",
        "create",
        "provision",
        "actor",
        "guard",
        "remove",
        "lock",
        "unlock",
        "sweep",
    ] {
        let help = s.run_in(main, &[sub, "--help"]);
        help.rc(0);
        help.out_has(&format!("Usage: worktree {sub}"));
        let mistyped = s.run_in(main, &[sub, "--bogus"]);
        mistyped.rc(2);
        mistyped.err_has("--bogus");
    }
    assert!(
        !exists(format!("{t}/project.worktrees")),
        "--help or a mistyped option placed a worktree"
    );
    assert_eq!(
        s.git(main, &["branch", "--list", "--", "-*"]),
        "",
        "--help or a mistyped option made a branch"
    );
    let listed = s.git(main, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        listed
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "--help or a mistyped option registered a worktree"
    );
}
