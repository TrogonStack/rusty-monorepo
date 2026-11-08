//! What a command concluded, in the vocabulary the process exits with.
//!
//! A job that goes red has to say which kind of red it is. A skill that scored badly is a
//! finding worth reading; a runner that was never installed is a broken job that measured
//! nothing at all. Collapsed onto one code the two are indistinguishable, and they ask
//! opposite things of whoever the alert reaches: read the diff, or repair the machine.

use serde::{Serialize, Serializer};

/// The signals a pass is taken down on.
///
/// The same three the harness termination handler installs for, because a pass cut short
/// is exactly a pass one of those reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationSignal {
    Interrupt,
    Terminate,
    Hangup,
}

impl TerminationSignal {
    pub const ALL: [Self; 3] = [Self::Interrupt, Self::Terminate, Self::Hangup];

    pub fn number(self) -> i32 {
        match self {
            Self::Interrupt => libc::SIGINT,
            Self::Terminate => libc::SIGTERM,
            Self::Hangup => libc::SIGHUP,
        }
    }
}

/// The code a command exits with, and what that code claims about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// The command did what it was asked; for a pass, every gate it was held to passed.
    Success,

    /// The command did not do what it was asked, and says why on stderr. Outside the eval
    /// passes this is the only way to fail, so it makes no claim about a skill being at
    /// fault, which is what separates it from `GateFailed`.
    Failed,

    /// The pass ran, and the thing under test failed a check that was asked of it. The one
    /// code that means the skill is what to go and look at.
    GateFailed,

    /// The cost ceiling refused a run, or spend went strictly past it. A run stopped by
    /// `--max-tool-calls-per-run` or `--max-turns-per-run` is not this: it is a run that
    /// did not complete, and lands wherever the gates put it.
    BudgetExhausted,

    /// The tool could not do its job, so nothing was learned about the skill either way.
    InfrastructureFailure,

    /// A signal cut the pass short before it reached a verdict.
    Interrupted(TerminationSignal),
}

impl ExitCode {
    /// The verdict of a check the operator asked for.
    pub fn from_gate(passed: bool) -> Self {
        if passed {
            Self::Success
        } else {
            Self::GateFailed
        }
    }

    /// Whether a command did what it was asked, for the commands with no gate to hold it to.
    pub fn from_success(succeeded: bool) -> Self {
        if succeeded {
            Self::Success
        } else {
            Self::Failed
        }
    }

    pub fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Failed => 1,
            Self::GateFailed => 1,
            Self::BudgetExhausted => 2,
            Self::InfrastructureFailure => 3,
            // The shell's own spelling for a process a signal took down, which is what a
            // caller observes here: `trg` hands the signal back to its default disposition
            // instead of returning a code of its own.
            Self::Interrupted(signal) => 128 + signal.number(),
        }
    }

    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// Where a budget stop lands among the codes a stage already reports.
    ///
    /// A stage that failed said so on its own terms, and the budget finding must not
    /// swallow that, so anything already reported still wins over a budget stop. A stage
    /// that passed said nothing about spend, so a pass the ceiling cut short cannot report
    /// success either.
    ///
    /// The same reasoning covers the codes a scored pass never reaches: a tool that broke
    /// measured nothing, and a pass a signal took down never finished, so neither of them
    /// has room for a finding about what the pass paid.
    ///
    /// Reported when the pass got less than it asked for, or paid more than it allowed:
    /// either a run was refused, or spend went strictly past the ceiling, which a single
    /// run can do on its own because admission is checked rather than reserved. A pass that
    /// lands exactly on its ceiling having refused nothing is neither, and succeeds: it did
    /// all the work and paid what it said it would.
    pub fn or_budget_exhausted(self, budget_exhausted: bool) -> Self {
        match self {
            Self::Success if budget_exhausted => Self::BudgetExhausted,
            other => other,
        }
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        u8::try_from(code.code()).map_or(Self::FAILURE, Self::from)
    }
}

impl Serialize for ExitCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.code())
    }
}

#[cfg(test)]
#[path = "exit_code_tests.rs"]
mod tests;
