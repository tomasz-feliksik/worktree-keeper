//! The exit contract every caller relies on: 0 done, 1 refused, 2 usage, 3 could
//! not check.
//!
//! "Could not check" is its own type, so that it can only travel upward. There is
//! no default value and no conversion to a success, and a function that returns
//! [`CannotTell`] cannot be read as "nothing found" by accident. A list that comes
//! back short because a probe failed is the failure this tool exists to prevent.

use std::fmt;

/// A check could not run, so the tool cannot tell whether acting is safe.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub struct CannotTell {
    message: String,
}

impl CannotTell {
    /// `message` is the whole report, printed as it is.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CannotTell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Why the answer was no, for an agent to act on without parsing prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    Other,
    UnmergedBranch,
}

/// Why a command stopped short of success.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum Stop {
    /// Exit 1: the tool looked, and the answer is no.
    Refused { kind: RefusalKind, message: String },
    /// Exit 2: the command line itself was wrong.
    Usage(String),
    /// Exit 3: the tool could not look.
    CannotTell(CannotTell),
}

impl Stop {
    pub fn refused(reason: impl fmt::Display) -> Self {
        Self::refused_as(RefusalKind::Other, reason)
    }

    pub fn refused_as(kind: RefusalKind, reason: impl fmt::Display) -> Self {
        Self::Refused {
            kind,
            message: format!("worktree: {reason}"),
        }
    }

    pub fn status(&self) -> u8 {
        match self {
            Self::Refused { .. } => 1,
            Self::Usage(_) => 2,
            Self::CannotTell(_) => 3,
        }
    }
}

impl From<CannotTell> for Stop {
    fn from(cannot_tell: CannotTell) -> Self {
        Self::CannotTell(cannot_tell)
    }
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused { message, .. } | Self::Usage(message) => f.write_str(message),
            Self::CannotTell(cannot_tell) => cannot_tell.fmt(f),
        }
    }
}

pub type Outcome<T = ()> = Result<T, Stop>;

/// The process exit status for a finished command.
#[must_use]
pub fn exit_status(outcome: &Outcome) -> u8 {
    match outcome {
        Ok(()) => 0,
        Err(stop) => stop.status(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_exits_zero() {
        assert_eq!(exit_status(&Ok(())), 0);
    }

    #[test]
    fn a_refusal_exits_one_with_the_tool_named() {
        let stop = Stop::refused("no");
        assert_eq!(stop.to_string(), "worktree: no");
        assert_eq!(exit_status(&Err(stop)), 1);
    }

    #[test]
    fn a_usage_error_exits_two() {
        assert_eq!(exit_status(&Err(Stop::Usage("bad".into()))), 2);
    }

    #[test]
    fn could_not_check_exits_three_through_question_mark() {
        fn inner() -> Result<(), CannotTell> {
            Err(CannotTell::new("cannot inspect"))
        }
        fn outer() -> Outcome {
            inner()?;
            Ok(())
        }
        let outcome = outer();
        assert_eq!(exit_status(&outcome), 3);
        assert_eq!(outcome.unwrap_err().to_string(), "cannot inspect");
    }
}
