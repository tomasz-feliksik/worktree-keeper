//! `worktree`: keeps agent git worktrees safe to create, inspect and remove.
//!
//! Skeleton only. The command surface is declared, and every command reports
//! that it cannot act. See docs/porting-guide.md.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The exit status for "could not check". An unimplemented command must never
/// read as "done" or "safe to remove" to a caller such as a `pre_remove` hook.
const CANNOT_TELL: u8 = 3;

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

fn main() -> ExitCode {
    let name = match Cli::parse().command {
        Command::Create { .. } => "create",
        Command::Provision { .. } => "provision",
        Command::Actor { .. } => "actor",
        Command::Guard { .. } => "guard",
        Command::Remove { .. } => "remove",
        Command::Sweep { .. } => "sweep",
    };
    eprintln!("worktree: {name} is not implemented yet; nothing was done");
    ExitCode::from(CANNOT_TELL)
}
