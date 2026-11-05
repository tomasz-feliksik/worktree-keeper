//! Messages to a person. A failure to print one must not change the exit status:
//! a guard that decided "may be removed" with its stdout closed still decided it.

use std::ffi::OsStr;
use std::fmt::Display;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Stdio;

use serde::Serialize;

use crate::outcome::Stop;

/// Where a command's narration goes. `create` narrates on stderr, because its
/// callers read stdout as the path and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sink {
    Stdout,
    Stderr,
}

impl Sink {
    /// The same destination, for a child process's stdout.
    pub fn stdio(self) -> Stdio {
        match self {
            Self::Stdout => Stdio::inherit(),
            Self::Stderr => Stdio::from(io::stderr()),
        }
    }
}

pub fn say(sink: Sink, message: impl Display) {
    let result = match sink {
        Sink::Stdout => writeln!(io::stdout(), "worktree: {message}"),
        Sink::Stderr => writeln!(io::stderr(), "worktree: {message}"),
    };
    // Nowhere left to report it, and the exit status is the contract.
    drop(result);
}

/// A name on stdout, as bytes, for a caller reading the output. Quiet about a
/// closed stdout, because there is nothing for such a caller to read anyway.
pub fn print_line(name: &OsStr) {
    drop(write_line(name));
}

/// A path on stdout, as bytes: the one output `create`'s callers wait for, so
/// failing to deliver it is reported.
pub fn print_path(path: &Path) -> io::Result<()> {
    write_line(path.as_os_str())
}

fn write_line(line: &OsStr) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")
}

/// A line for a person reading a question's answer, with no `worktree:` prefix.
pub fn print_text_line(line: &str) {
    drop(writeln!(io::stdout(), "{line}"));
}

/// One JSON document on stdout. A caller that gets no document has no answer, so failing
/// to deliver it is reported.
pub fn print_json(document: &impl Serialize) -> Result<(), Stop> {
    let mut stdout = io::stdout();
    serde_json::to_writer_pretty(&mut stdout, document)
        .map_err(|error| error.to_string())
        .and_then(|()| writeln!(stdout).map_err(|error| error.to_string()))
        .map_err(|error| Stop::refused(format!("cannot print the answer: {error}")))
}
