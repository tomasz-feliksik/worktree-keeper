//! `worktree`: keeps agent git worktrees safe to create, inspect and remove.
//!
//! See docs/porting-guide.md. Exit status: 0 done, 1 refused, 2 usage, 3 could not
//! check. Nothing else may be returned.

mod commands;
mod git;
mod outcome;
mod paths;
mod say;
mod submodules;
mod temp;
#[cfg(test)]
mod testutil;
mod worktree;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use commands::guard::OwnChanges;
use commands::remove::BranchFate;
use commands::sweep::Mode;
use outcome::{Outcome, exit_status};
use say::Sink;

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

fn run(command: Command) -> Outcome {
    match command {
        Command::Create { branch, base } => commands::create::run(&branch, base.as_deref()),
        Command::Provision { path } => {
            commands::provision::run(&path.unwrap_or_else(|| ".".into()), Sink::Stdout)
        }
        Command::Actor { dir } => {
            commands::actor::run(dir);
            Ok(())
        }
        Command::Guard { force, path } => commands::guard::run(path, OwnChanges::from_force(force)),
        Command::Remove {
            force,
            keep_branch,
            delete_remote,
            path,
        } => {
            let fate = if keep_branch {
                BranchFate::Keep
            } else if delete_remote {
                BranchFate::DeleteLocalAndRemote
            } else {
                BranchFate::DeleteLocal
            };
            commands::remove::run(&path, OwnChanges::from_force(force), fate)
        }
        Command::Sweep {
            apply,
            repos,
            roots,
        } => {
            let mode = if apply { Mode::Apply } else { Mode::DryRun };
            commands::sweep::run(mode, repos, roots)
        }
    }
}

fn main() -> ExitCode {
    let outcome = run(Cli::parse().command);
    if let Err(stop) = &outcome {
        // Nowhere left to report a failure to report, and the status still stands.
        drop(writeln!(io::stderr(), "{stop}"));
    }
    ExitCode::from(exit_status(&outcome))
}
