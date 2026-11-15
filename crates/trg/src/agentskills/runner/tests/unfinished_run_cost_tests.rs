use super::super::{runner_failure_outcome, timeout_outcome, Runner};
use crate::agentskills::budget::RunCost;

#[test]
fn a_failed_run_of_a_harness_that_prices_nothing_still_names_it() {
    let outcome = runner_failure_outcome(Runner::CursorAgent, 10, Some(1), String::new());

    assert_eq!(
        outcome.cost,
        Some(RunCost::Unpriced {
            harness: "cursor-agent".to_string()
        })
    );
}

#[test]
fn a_run_the_clock_ended_says_what_its_harness_can_say_about_cost() {
    let outcome = timeout_outcome(Runner::Codex, 10, None);

    assert_eq!(
        outcome.cost,
        Some(RunCost::Unpriced {
            harness: "codex".to_string()
        })
    );
}

#[test]
fn a_failed_run_of_a_pricing_harness_carries_no_price_rather_than_naming_it() {
    let outcome = runner_failure_outcome(Runner::ClaudeCode, 10, Some(1), String::new());

    assert_eq!(outcome.cost, None);
}
