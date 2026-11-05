//! The black-box suite, for `cargo mutants`, which only sees `cargo test`.
//!
//! Ignored in a plain `cargo test`: `just parity` is its gate, and the crane source
//! filter leaves `tests/parity/test.sh` out of the Nix build. `just mutants` runs it
//! with `--include-ignored`, so that a mutant is judged by the suite that holds the
//! guarantees rather than by the unit tests alone.

use std::process::Command;

#[test]
#[ignore = "run by `just parity`, and by `just mutants` with --include-ignored"]
fn the_black_box_suite_passes() {
    let output = Command::new("bash")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/parity/test.sh"))
        .env("WORKTREE", env!("CARGO_BIN_EXE_worktree"))
        .output()
        .expect("bash should start");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
