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
fn a_command_that_failed_is_not_a_failed_gate() {
    assert_ne!(ExitCode::Failed, ExitCode::GateFailed);
    assert_eq!(ExitCode::Failed.code(), 1);
    assert_eq!(ExitCode::from_success(true), ExitCode::Success);
    assert_eq!(ExitCode::from_success(false), ExitCode::Failed);
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
