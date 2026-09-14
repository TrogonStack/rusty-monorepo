//! What a pass concluded, in the vocabulary the process exits with.
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

/// The code a pass exits with, and what that code claims about the pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// The pass ran and every gate it was held to passed.
    Success,

    /// The pass ran, and the thing under test failed a check that was asked of it. The one
    /// code that means the skill is what to go and look at.
    GateFailed,

    /// The cost ceiling refused a run, or spend went strictly past it.
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

    pub fn code(self) -> i32 {
        match self {
            Self::Success => 0,
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

impl From<ExitCode> for i32 {
    fn from(code: ExitCode) -> Self {
        code.code()
    }
}

impl Serialize for ExitCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers are the contract every CI job already reads, so they are pinned rather
    /// than derived from the order the variants happen to be written in.
    #[test]
    fn the_codes_ci_already_reads_keep_their_numbers() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::GateFailed.code(), 1);
        assert_eq!(ExitCode::BudgetExhausted.code(), 2);
    }

    #[test]
    fn a_tool_that_broke_is_not_one_of_the_codes_a_scored_pass_reports() {
        let scored = [ExitCode::Success, ExitCode::GateFailed, ExitCode::BudgetExhausted];
        assert!(!scored.contains(&ExitCode::InfrastructureFailure));
        assert_eq!(ExitCode::InfrastructureFailure.code(), 3);
    }

    #[test]
    fn an_interrupted_pass_reports_what_the_shell_reports() {
        assert_eq!(ExitCode::Interrupted(TerminationSignal::Interrupt).code(), 130);
        assert_eq!(ExitCode::Interrupted(TerminationSignal::Terminate).code(), 143);
        assert_eq!(ExitCode::Interrupted(TerminationSignal::Hangup).code(), 129);
    }

    /// The rule the budget stop was written under: a stage that already failed said so on
    /// its own terms, and a budget finding must not overwrite it.
    #[test]
    fn a_gate_failure_still_wins_over_a_budget_stop() {
        assert_eq!(ExitCode::GateFailed.or_budget_exhausted(true), ExitCode::GateFailed);
    }

    #[test]
    fn a_pass_the_ceiling_cut_short_cannot_report_success() {
        assert_eq!(ExitCode::Success.or_budget_exhausted(true), ExitCode::BudgetExhausted);
    }

    #[test]
    fn a_pass_that_refused_nothing_and_stayed_under_its_ceiling_succeeds() {
        assert_eq!(ExitCode::Success.or_budget_exhausted(false), ExitCode::Success);
    }

    /// A pass whose tool broke measured nothing, so it has no room for a finding about
    /// what it paid.
    #[test]
    fn a_broken_tool_still_wins_over_a_budget_stop() {
        assert_eq!(
            ExitCode::InfrastructureFailure.or_budget_exhausted(true),
            ExitCode::InfrastructureFailure
        );
    }

    #[test]
    fn an_interrupted_pass_still_wins_over_a_budget_stop() {
        let interrupted = ExitCode::Interrupted(TerminationSignal::Interrupt);
        assert_eq!(interrupted.or_budget_exhausted(true), interrupted);
    }

    /// The reports carry the code a caller would have read off the process, so the two
    /// cannot drift apart.
    #[test]
    fn a_report_carries_the_code_the_process_exits_with() {
        assert_eq!(serde_json::to_string(&ExitCode::InfrastructureFailure).unwrap(), "3");
        assert_eq!(serde_json::to_string(&ExitCode::GateFailed).unwrap(), "1");
    }

    #[test]
    fn a_gate_reports_its_own_verdict() {
        assert_eq!(ExitCode::from_gate(true), ExitCode::Success);
        assert_eq!(ExitCode::from_gate(false), ExitCode::GateFailed);
    }

    /// Every signal the harness termination handler installs for has a code, because each
    /// of them is a way a pass ends without a verdict.
    #[test]
    fn every_signal_a_pass_is_taken_down_on_has_a_code() {
        for signal in TerminationSignal::ALL {
            assert_eq!(ExitCode::Interrupted(signal).code(), 128 + signal.number());
        }
    }
}
