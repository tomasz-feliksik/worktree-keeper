//! Running `git` as a program.
//!
//! [`Git::probe`] on any failure stops loudly with
//! the command, the path it was about and git's own words. The one probe whose
//! failure used to be silent is the one that decides whether a commit is about to
//! be lost; a probe that cannot answer is "unknown", and unknown refuses.
//!
//! git inherits the caller's environment on purpose, so that it sees the same
//! configuration as the user's shell, except for what ties it to one repository: see
//! [`REPOSITORY_LOCATION`].

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::outcome::CannotTell;
use crate::say::Sink;

/// What ties git to one repository, from `git rev-parse --local-env-vars` without the
/// configuration entries. Git hooks export some of these, and they outrank `-C`, so left in
/// place a question about one worktree is answered about another, and `remove` deletes it.
/// The tool always says where it means.
const REPOSITORY_LOCATION: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_GRAFT_FILE",
    "GIT_SHALLOW_FILE",
    "GIT_PREFIX",
];

/// One `git` command, built up and then run in exactly one of the ways below.
#[must_use]
pub struct Git {
    command: Command,
    shown: String,
}

impl Git {
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new("git");
        for name in REPOSITORY_LOCATION {
            command.env_remove(name);
        }
        let mut shown = String::from("git");
        for arg in args {
            let arg = arg.as_ref();
            command.arg(arg);
            shown.push(' ');
            shown.push_str(&arg.to_string_lossy());
        }
        Self { command, shown }
    }

    /// `git -C <dir> <args>`.
    pub fn at<I, S>(dir: &Path, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut full = vec![OsString::from("-C"), dir.as_os_str().to_owned()];
        full.extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        Self::new(full)
    }

    /// `git --git-dir=<clone> --work-tree=/ <args>`: how a clone is read.
    ///
    /// `--work-tree=/` is the fix for the silent exit 128. After a deinit, a nested
    /// submodule's gitdir keeps `core.worktree` pointing at a directory that no
    /// longer exists, and git refuses to run there at all, even for `log`, which
    /// never touches a work tree. Overriding it with any directory that exists lets
    /// the history be read. Addressed by `--git-dir`, never through a submodule's
    /// gitfile, whose relative path goes stale when the repository moves.
    pub fn in_clone<I, S>(clone: &Path, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut full = vec![flag("--git-dir=", clone), OsString::from("--work-tree=/")];
        full.extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        Self::new(full)
    }

    /// Reads and writes this index instead of the repository's.
    pub fn index_file(mut self, index: &Path) -> Self {
        self.command.env("GIT_INDEX_FILE", index);
        self
    }

    /// Writes new objects to `scratch`, reading existing ones from `objects` too, so that
    /// asking a question leaves the repository's object store as it was.
    pub fn objects_in(mut self, scratch: &Path, objects: &Path) -> Self {
        self.command.env("GIT_OBJECT_DIRECTORY", scratch);
        self.command
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", quoted_for_git(objects));
        self
    }

    /// Runs git from `dir`, for a command that must not be told with `-C`.
    pub fn current_dir(mut self, dir: &Path) -> Self {
        self.command.current_dir(dir);
        self
    }

    /// The command's stdout. Any failure to answer, git's own or failing to
    /// start it at all, is [`CannotTell`], naming `about`.
    pub fn probe(mut self, about: &Path) -> Result<Vec<u8>, CannotTell> {
        match self.command.output() {
            Ok(output) if output.status.success() => Ok(output.stdout),
            Ok(output) => Err(probe_failure(about, &self.shown, &output)),
            Err(error) => Err(could_not_start(about, &self.shown, &error)),
        }
    }

    /// The command's stdout if it succeeded, and `None` if git answered no. For
    /// the few questions where a failure is itself the answer: the caller has
    /// decided that, here, "git said no" is not "unknown".
    pub fn answer(mut self, about: &Path) -> Result<Option<Vec<u8>>, CannotTell> {
        let output = self
            .command
            .output()
            .map_err(|error| could_not_start(about, &self.shown, &error))?;
        Ok(output.status.success().then_some(output.stdout))
    }

    /// Whether the command succeeded. See [`Git::answer`].
    pub fn succeeds(self, about: &Path) -> Result<bool, CannotTell> {
        Ok(self.answer(about)?.is_some())
    }

    /// The command's exit code, with its output discarded, for a command whose
    /// codes mean different things. `None` if a signal killed it.
    pub fn exit_code(mut self, about: &Path) -> Result<Option<i32>, CannotTell> {
        let output = self
            .command
            .output()
            .map_err(|error| could_not_start(about, &self.shown, &error))?;
        Ok(output.status.code())
    }

    /// The exit code and stdout, for a command whose codes mean different things and
    /// whose output is the answer.
    pub fn code_and_output(mut self, about: &Path) -> Result<(Option<i32>, Vec<u8>), CannotTell> {
        let output = self
            .command
            .output()
            .map_err(|error| could_not_start(about, &self.shown, &error))?;
        Ok((output.status.code(), output.stdout))
    }

    /// Runs an action with git's stdout on `sink` and its stderr on ours, and
    /// returns whether it succeeded. Git's words are the report.
    pub fn act(mut self, sink: Sink) -> io::Result<bool> {
        Ok(self.command.stdout(sink.stdio()).status()?.success())
    }
}

/// Command output as a path or similar, as `$(...)` would give it: bytes intact,
/// and only the trailing newlines removed.
#[must_use]
pub fn trimmed(mut output: Vec<u8>) -> OsString {
    while output.last() == Some(&b'\n') {
        output.pop();
    }
    OsString::from_vec(output)
}

/// [`trimmed`], as a path.
#[must_use]
pub fn trimmed_path(output: Vec<u8>) -> PathBuf {
    PathBuf::from(trimmed(output))
}

/// A `git` argument of the form `<flag><path>`, such as `--git-dir=<path>`.
#[must_use]
pub fn flag(flag: &str, path: &Path) -> OsString {
    let mut joined = OsString::from(flag);
    joined.push(path.as_os_str());
    joined
}

/// Git splits this list at colons unless an entry is quoted, C-style.
fn quoted_for_git(path: &Path) -> OsString {
    let mut bytes = vec![b'"'];
    for byte in path.as_os_str().as_bytes() {
        if matches!(byte, b'"' | b'\\') {
            bytes.push(b'\\');
        }
        bytes.push(*byte);
    }
    bytes.push(b'"');
    OsString::from_vec(bytes)
}

/// Lossy text, only for showing to a person.
#[must_use]
pub fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_end().to_owned()
}

/// The records of NUL-terminated `-z` output.
pub fn records(output: &[u8]) -> impl Iterator<Item = &[u8]> {
    output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
}

/// One `<mode> <object> <stage>\t<path>` or `<mode> <type> <object>\t<path>`
/// record split at the tab, the only safe place to split: a path can hold spaces.
#[must_use]
pub fn split_record(record: &[u8]) -> Option<(&[u8], &OsStr)> {
    let tab = record.iter().position(|byte| *byte == b'\t')?;
    Some((&record[..tab], OsStr::from_bytes(&record[tab + 1..])))
}

fn could_not_start(about: &Path, command: &str, error: &io::Error) -> CannotTell {
    CannotTell::new(format!(
        "worktree: cannot inspect {} (git could not run) — refusing, because an\n\
         \x20         unanswerable probe is not the same as a safe one.\n\
         \x20         command: {command}\n\
         \x20         {error}",
        about.display()
    ))
}

fn probe_failure(about: &Path, command: &str, output: &Output) -> CannotTell {
    let exit = output.status.code().map_or_else(
        || "killed by a signal".to_owned(),
        |code| format!("git exit {code}"),
    );
    let mut words = String::new();
    for stream in [&output.stdout, &output.stderr] {
        for line in String::from_utf8_lossy(stream).lines() {
            words.push_str("\n          ");
            words.push_str(line);
        }
    }
    CannotTell::new(format!(
        "worktree: cannot inspect {} ({exit}) — refusing, because an\n\
         \x20         unanswerable probe is not the same as a safe one.\n\
         \x20         command: {command}{words}",
        about.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{Outcome, Stop, exit_status};
    use crate::testutil::Scratch;

    #[test]
    fn a_failed_git_command_becomes_exit_three_naming_the_command_and_path() {
        let scratch = Scratch::new();
        let not_a_repo = scratch.path().join("not-a-repo");
        std::fs::create_dir(&not_a_repo).unwrap();

        let run = || -> Outcome {
            Git::at(&not_a_repo, ["log"]).probe(&not_a_repo)?;
            Ok(())
        };
        let outcome = run();

        assert_eq!(exit_status(&outcome), 3);
        let report = outcome.unwrap_err().to_string();
        assert!(report.contains("cannot inspect"), "{report}");
        assert!(
            report.contains(&not_a_repo.display().to_string()),
            "{report}"
        );
        assert!(report.contains("command: git -C"), "{report}");
        assert!(report.contains("git exit 128"), "{report}");
        assert!(
            report.contains("not a git repository"),
            "git's words: {report}"
        );
    }

    #[test]
    fn a_probe_returns_stdout_only_when_git_succeeds() {
        let output = Git::new(["--version"]).probe(Path::new(".")).unwrap();
        assert!(trimmed(output).to_string_lossy().starts_with("git version"));
    }

    #[test]
    fn an_answer_of_no_is_none_rather_than_a_failure() {
        let scratch = Scratch::new();
        let answer = Git::new(["rev-parse", "--git-dir"])
            .current_dir(scratch.path())
            .answer(scratch.path())
            .unwrap();
        assert_eq!(answer, None);
    }

    #[test]
    fn git_that_cannot_start_is_could_not_tell_for_every_question() {
        let missing = Path::new("/nonexistent-working-directory");
        let git = || Git::new(["--version"]).current_dir(missing);
        assert!(matches!(git().probe(missing), Err(CannotTell { .. })));
        assert!(matches!(git().answer(missing), Err(CannotTell { .. })));
        assert!(matches!(git().succeeds(missing), Err(CannotTell { .. })));
        assert!(matches!(git().exit_code(missing), Err(CannotTell { .. })));
        let stop = Stop::from(git().probe(missing).unwrap_err());
        assert_eq!(stop.status(), 3);
    }

    #[test]
    fn trimming_removes_only_trailing_newlines() {
        assert_eq!(trimmed(b"a b\n\n".to_vec()), OsString::from("a b"));
        assert_eq!(trimmed(b" a\nb ".to_vec()), OsString::from(" a\nb "));
    }

    #[test]
    fn a_record_splits_at_the_tab_so_a_path_keeps_its_spaces() {
        let (fields, path) = split_record(b"160000 abc 0\tlibs/my sub").unwrap();
        assert_eq!(fields, b"160000 abc 0");
        assert_eq!(path, OsStr::new("libs/my sub"));
        assert_eq!(split_record(b"no tab"), None);
    }

    #[test]
    fn records_are_nul_terminated_and_keep_newlines() {
        let found: Vec<_> = records(b"a\nb\0c\0").collect();
        assert_eq!(found, [b"a\nb".as_slice(), b"c".as_slice()]);
    }
}
