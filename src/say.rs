//! Messages to a person. A failure to print one must not change the exit status:
//! a guard that decided "may be removed" with its stdout closed still decided it.

use std::fmt::Display;
use std::io::{self, Write};

/// Where a command's narration goes. `create` narrates on stderr, because its
/// callers read stdout as the path and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sink {
    Stdout,
    Stderr,
}

pub fn say(sink: Sink, message: impl Display) {
    let result = match sink {
        Sink::Stdout => writeln!(io::stdout(), "worktree: {message}"),
        Sink::Stderr => writeln!(io::stderr(), "worktree: {message}"),
    };
    // Nowhere left to report it, and the exit status is the contract.
    drop(result);
}
