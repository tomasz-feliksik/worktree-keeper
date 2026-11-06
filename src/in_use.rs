//! Is a process running inside this worktree? Answers measured with real callers, in
//! docs/internals.md § In-use detection:
//!
//! - The caller's own process and its ancestors never count. Descendants of an ancestor
//!   do: sibling panes descend from the same tmux server, and they are what this looks
//!   for.
//! - An entry that cannot be read does not count. On a normal machine that is another
//!   user's process (measured: 543 of 1334), so "cannot tell" there would mean always
//!   exit 3.
//! - Detection proves it works by reading its own working directory. If it cannot, it
//!   cannot tell, and that is exit 3.

use std::fmt;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::{fs, path::PathBuf};

use crate::outcome::CannotTell;

/// A process with its working directory inside the worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub name: String,
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (pid {})", self.name, self.pid)
    }
}

/// The processes holding `worktree`, other than the caller and its ancestors.
pub fn holders(worktree: &Path) -> Result<Vec<Holder>, CannotTell> {
    #[cfg(target_os = "linux")]
    {
        scan_proc(Path::new("/proc"), worktree)
    }
    #[cfg(not(target_os = "linux"))]
    {
        scan_lsof(worktree)
    }
}

/// Reads `/proc/<pid>/cwd`. Takes the root so that a test can show what happens when
/// detection cannot run.
#[cfg(target_os = "linux")]
fn scan_proc(proc: &Path, worktree: &Path) -> Result<Vec<Holder>, CannotTell> {
    let cannot = |error: &dyn fmt::Display| {
        CannotTell::new(format!(
            "worktree: cannot tell whether a process is running in {}: reading {} failed: {error}",
            worktree.display(),
            proc.display()
        ))
    };
    fs::read_link(proc.join("self/cwd")).map_err(|error| cannot(&error))?;
    let own = own_chain(proc);
    let mut found = Vec::new();
    for entry in fs::read_dir(proc).map_err(|error| cannot(&error))? {
        let entry = entry.map_err(|error| cannot(&error))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let cwd: Option<PathBuf> = fs::read_link(entry.path().join("cwd")).ok();
        if holds(cwd.as_deref(), worktree) && !own.contains(&pid) {
            let name = fs::read_to_string(entry.path().join("comm"))
                .map_or_else(|_| "unknown".to_owned(), |name| name.trim_end().to_owned());
            found.push(Holder { pid, name });
        }
    }
    found.sort_by_key(|holder| holder.pid);
    Ok(found)
}

#[cfg(target_os = "linux")]
const MAX_ANCESTORS: usize = 1024;

#[cfg(target_os = "linux")]
fn own_chain(proc: &Path) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut at = String::from("self");
    // Bounded, so that a /proc that lies cannot hang a hook. Stopping early only ever
    // leaves an ancestor to be reported as a holder, which refuses: the safe direction.
    for _ in 0..MAX_ANCESTORS {
        let Ok(stat) = fs::read_to_string(proc.join(&at).join("stat")) else {
            break;
        };
        let Some((pid, parent)) = pid_and_parent(&stat) else {
            break;
        };
        chain.push(pid);
        if parent == 0 {
            break;
        }
        at = parent.to_string();
    }
    chain
}

/// `<pid> (<name>) <state> <ppid> …`. The name can hold spaces and parentheses, so the
/// fields are counted from the last `)`.
#[cfg(target_os = "linux")]
fn pid_and_parent(stat: &str) -> Option<(u32, u32)> {
    let pid = stat.split(' ').next()?.parse().ok()?;
    let after_name = &stat[stat.rfind(')')? + 1..];
    let parent = after_name.split_whitespace().nth(1)?.parse().ok()?;
    Some((pid, parent))
}

/// Whether a working directory, if it could be read, is inside the worktree.
fn holds(cwd: Option<&Path>, worktree: &Path) -> bool {
    cwd.is_some_and(|cwd| cwd.starts_with(worktree))
}

/// macOS has no `/proc`. The package puts `lsof` on its `PATH`.
#[cfg(not(target_os = "linux"))]
fn scan_lsof(worktree: &Path) -> Result<Vec<Holder>, CannotTell> {
    use std::process::Command;
    let cannot = |why: String| {
        CannotTell::new(format!(
            "worktree: cannot tell whether a process is running in {}: {why}",
            worktree.display()
        ))
    };
    let output = Command::new("lsof")
        .args(["-d", "cwd", "-Fpcn"])
        .output()
        .map_err(|error| cannot(format!("lsof could not run: {error}")))?;
    if output.stdout.is_empty() {
        return Err(cannot("lsof reported no processes at all".to_owned()));
    }
    let own = own_chain_by_ps();
    Ok(
        parse_lsof(&String::from_utf8_lossy(&output.stdout), worktree)
            .into_iter()
            .filter(|holder| !own.contains(&holder.pid))
            .collect(),
    )
}

#[cfg(not(target_os = "linux"))]
fn own_chain_by_ps() -> Vec<u32> {
    use std::process::Command;
    let mut chain = vec![std::process::id()];
    while let Some(at) = chain.last().copied().filter(|pid| *pid > 1) {
        let parent = Command::new("ps")
            .args(["-o", "ppid=", "-p", &at.to_string()])
            .output()
            .ok()
            .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok());
        match parent {
            Some(parent) => chain.push(parent),
            None => break,
        }
    }
    chain
}

/// `lsof -F` prints one field a line: `p<pid>`, then `c<command>`, `f<fd>`, `n<path>`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_lsof(output: &str, worktree: &Path) -> Vec<Holder> {
    let mut found = Vec::new();
    let (mut pid, mut name) = (None, String::new());
    for line in output.lines() {
        let Some((field, value)) = line.split_at_checked(1) else {
            continue;
        };
        match field {
            "p" => pid = value.parse().ok(),
            "c" => value.clone_into(&mut name),
            "n" if holds(Some(Path::new(value)), worktree) => {
                if let Some(pid) = pid {
                    found.push(Holder {
                        pid,
                        name: name.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;
    use std::process::{Command, Stdio};

    #[test]
    fn a_cwd_inside_counts_and_a_shared_prefix_or_unreadable_one_does_not() {
        let wt = Path::new("/root/wt");
        assert!(holds(Some(wt), wt));
        assert!(holds(Some(Path::new("/root/wt/sub/dir")), wt));
        assert!(!holds(Some(Path::new("/root/wt-other")), wt));
        assert!(!holds(None, wt));
    }

    #[test]
    fn lsof_output_names_the_processes_inside_the_worktree() {
        let output = "p1\ncsystemd\nfcwd\nn/proc/1/cwd (readlink: Permission denied)\n\
                      p42\ncmy agent\nfcwd\nn/wt/sub\np43\ncother\nfcwd\nn/elsewhere\n";
        assert_eq!(
            parse_lsof(output, Path::new("/wt")),
            [Holder {
                pid: 42,
                name: "my agent".into()
            }]
        );
    }

    #[test]
    fn a_holder_is_shown_by_name_and_pid() {
        let holder = Holder {
            pid: 7,
            name: "claude".into(),
        };
        assert_eq!(holder.to_string(), "claude (pid 7)");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_fields_are_counted_from_the_end_of_the_name() {
        assert_eq!(pid_and_parent("12 (a) b) c) S 34 56"), Some((12, 34)));
        assert_eq!(pid_and_parent("12 (sleep) S 1 12"), Some((12, 1)));
        assert_eq!(pid_and_parent("garbage"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_inside_is_found_and_this_process_is_not() {
        let scratch = Scratch::new();
        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(scratch.path())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let found = holders(scratch.path());
        child.kill().unwrap();
        child.wait().unwrap();
        let found = found.unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].pid, child.id());
        assert_eq!(found[0].name, "sleep");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_and_its_ancestors_do_not_count_as_holders() {
        // The test runner's own cwd holds the crate, and so do its ancestors' at times.
        let own = own_chain(Path::new("/proc"));
        assert_eq!(own.first(), Some(&std::process::id()));
        assert!(own.len() > 1, "at least a parent: {own:?}");
        let cwd = std::env::current_dir().unwrap();
        let found = holders(&cwd).unwrap();
        assert!(
            found.iter().all(|holder| !own.contains(&holder.pid)),
            "{found:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detection_that_cannot_read_its_own_entry_cannot_tell() {
        let scratch = Scratch::new();
        // A directory with no self/cwd: what a /proc that does not work looks like.
        let found = scan_proc(scratch.path(), Path::new("/wt"));
        assert!(found.unwrap_err().to_string().contains("cannot tell"));
        let missing = scan_proc(&scratch.path().join("no-proc"), Path::new("/wt"));
        assert!(missing.is_err());
    }
}
