//! `worktree`: keeps agent git worktrees safe to create, inspect and remove.
//!
//! See docs/internals.md. Exit status: 0 done, 1 refused, 2 usage, 3 could not
//! check. Nothing else may be returned.

mod commands;
mod git;
mod in_use;
mod outcome;
mod paths;
mod placement;
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

use commands::check::Format;
use commands::guard::Rules;
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
    /// Add <parent>/<repo>.worktrees/<slug>, then provision it
    Create {
        branch: String,
        base: Option<String>,
    },
    /// Initialise submodules, then apply gated direnv trust
    Provision { path: Option<PathBuf> },
    /// Print the identity of the worktree containing a directory
    Actor { dir: Option<PathBuf> },
    /// List this repository's worktrees, or a directory's, and whether each could be removed; changes nothing
    Status {
        #[arg(long)]
        json: bool,
        path: Option<PathBuf>,
    },
    /// Ask whether a worktree may be removed; changes nothing
    Check {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        ignore_in_use: bool,
        path: PathBuf,
    },
    /// Check whether a worktree may be removed, then clear stale metadata; never removes it
    Guard {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        ignore_in_use: bool,
        path: Option<PathBuf>,
    },
    /// Guard, then remove the worktree and its merged branch
    Remove {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        ignore_in_use: bool,
        #[arg(long)]
        keep_branch: bool,
        #[arg(long)]
        delete_remote: bool,
        path: PathBuf,
    },
    /// Mark a worktree as occupied, with git worktree lock
    Lock {
        #[arg(long)]
        reason: Option<String>,
        path: Option<PathBuf>,
    },
    /// Release a worktree marked as occupied
    Unlock { path: Option<PathBuf> },
    /// Find leftovers; delete them only with --apply
    Sweep {
        #[arg(long)]
        apply: bool,
        #[arg(long = "repo")]
        repos: Vec<PathBuf>,
        #[arg(value_name = "DIR")]
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
        Command::Status { json, path } => commands::status::run(path, format_of(json)),
        Command::Check {
            json,
            force,
            ignore_in_use,
            path,
        } => commands::check::run(
            &path,
            Rules::from_flags(force, ignore_in_use),
            format_of(json),
        ),
        Command::Guard {
            force,
            ignore_in_use,
            path,
        } => commands::guard::run(path, Rules::from_flags(force, ignore_in_use)),
        Command::Lock { reason, path } => commands::lock::lock(path, reason),
        Command::Unlock { path } => commands::lock::unlock(path),
        Command::Remove {
            force,
            ignore_in_use,
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
            commands::remove::run(&path, Rules::from_flags(force, ignore_in_use), fate)
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

fn format_of(json: bool) -> Format {
    if json { Format::Json } else { Format::Text }
}

fn main() -> ExitCode {
    let outcome = run(Cli::parse().command);
    if let Err(stop) = &outcome {
        // Nowhere left to report a failure to report, and the status still stands.
        drop(writeln!(io::stderr(), "{stop}"));
    }
    ExitCode::from(exit_status(&outcome))
}
