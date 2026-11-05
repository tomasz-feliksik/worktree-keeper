//! `check`: may this worktree be removed? The same rules as `guard`, because it is the
//! same code, and it changes nothing on disk.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::commands::guard::{self, Rules};
use crate::outcome::{Outcome, Stop};
use crate::say::{self, Sink, say};
use crate::worktree::LinkedWorktree;

/// See docs/json-schema.md before changing what this describes.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Removable,
    Refused,
    Unknown,
}

impl Verdict {
    /// What a stop means to someone asking. Exhaustive, so that a new kind of stop has
    /// to be given a meaning here rather than falling into "refused" or "removable".
    #[must_use]
    pub fn of(stop: &Stop) -> Self {
        match stop {
            Stop::Refused { .. } => Self::Refused,
            Stop::CannotTell(_) | Stop::Usage(_) => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub kind: String,
    pub message: String,
}

impl Reason {
    #[must_use]
    pub fn of(stop: &Stop) -> Self {
        Self {
            kind: stop.kind_name().to_owned(),
            message: stop.to_string().trim_start_matches("worktree: ").to_owned(),
        }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    path: String,
    verdict: Verdict,
    reasons: &'a [Reason],
}

/// How a question's answer is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

pub fn run(path: &Path, rules: Rules, format: Format) -> Outcome {
    let (shown_path, answer) = assess(path, rules);
    if format == Format::Json {
        let reasons: Vec<Reason> = answer.as_ref().err().map(Reason::of).into_iter().collect();
        let verdict = answer
            .as_ref()
            .err()
            .map_or(Verdict::Removable, Verdict::of);
        say::print_json(&Report {
            schema_version: SCHEMA_VERSION,
            path: shown_path.display().to_string(),
            verdict,
            reasons: &reasons,
        })?;
    } else if answer.is_ok() {
        say(
            Sink::Stdout,
            format_args!("{} may be removed", shown_path.display()),
        );
    }
    answer
}

fn assess(path: &Path, rules: Rules) -> (PathBuf, Outcome) {
    match LinkedWorktree::resolve(path) {
        Ok(worktree) => {
            let answer = guard::assess(&worktree, rules).map(drop);
            (worktree.path, answer)
        }
        Err(stop) => (path.to_path_buf(), Err(stop)),
    }
}
