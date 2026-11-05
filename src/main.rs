//! `worktree`: keeps agent git worktrees safe to create, inspect and remove.
//!
//! See docs/porting-guide.md. Exit status: 0 done, 1 refused, 2 usage, 3 could not
//! check. Nothing else may be returned.

#[allow(dead_code)] // Reached as each command is ported.
mod git;
#[allow(dead_code)]
mod outcome;
#[allow(dead_code)]
mod paths;
#[allow(dead_code)]
mod say;
#[cfg(test)]
mod testutil;
#[allow(dead_code)]
mod worktree;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use outcome::{CannotTell, Outcome, Stop, exit_status};

#[derive(Parser)]
#[command(
    name = "worktree",
    version,
    about = "Keeps agent git worktrees safe to create, inspect and remove"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add <root>/<repo>/<slug>, then provision it
    Create {
        branch: String,
        base: Option<String>,
    },
    /// Initialise submodules, then apply gated direnv trust
    Provision { path: Option<PathBuf> },
    /// Print the identity of the worktree containing a directory
    Actor { dir: Option<PathBuf> },
    /// Check whether a worktree may be removed, then clear stale metadata; never removes it
    Guard {
        #[arg(long)]
        force: bool,
        path: Option<PathBuf>,
    },
    /// Guard, then remove the worktree and its merged branch
    Remove {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        keep_branch: bool,
        #[arg(long)]
        delete_remote: bool,
        path: PathBuf,
    },
    /// Find leftovers; delete them only with --apply
    Sweep {
        #[arg(long)]
        apply: bool,
        #[arg(long = "repo")]
        repos: Vec<PathBuf>,
        roots: Vec<PathBuf>,
    },
}

fn run(command: &Command) -> Outcome {
    let name = match command {
        Command::Create { .. } => "create",
        Command::Provision { .. } => "provision",
        Command::Actor { .. } => "actor",
        Command::Guard { .. } => "guard",
        Command::Remove { .. } => "remove",
        Command::Sweep { .. } => "sweep",
    };
    Err(Stop::CannotTell(CannotTell::new(format!(
        "worktree: {name} is not implemented yet; nothing was done"
    ))))
}

fn main() -> ExitCode {
    let outcome = run(&Cli::parse().command);
    if let Err(stop) = &outcome {
        // Nowhere left to report a failure to report, and the status still stands.
        drop(writeln!(io::stderr(), "{stop}"));
    }
    ExitCode::from(exit_status(&outcome))
}
