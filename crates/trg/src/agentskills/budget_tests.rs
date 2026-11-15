use super::*;

#[test]
fn a_ceiling_of_zero_is_refused() {
    assert!(CostCeiling::parse(0.0).is_err());
}

#[test]
fn a_negative_ceiling_is_refused() {
    assert!(CostCeiling::parse(-5.0).is_err());
}

#[test]
fn a_ceiling_that_is_not_a_finite_number_is_refused() {
    assert!(CostCeiling::parse(f64::NAN).is_err());
    assert!(CostCeiling::parse(f64::INFINITY).is_err());
    assert!(CostCeiling::parse(f64::NEG_INFINITY).is_err());
}

#[test]
fn a_cost_ceiling_is_parsed_from_the_command_line_form() {
    assert_eq!("2.5".parse::<CostCeiling>().unwrap().usd(), 2.5);
    assert!(
        "nan".parse::<CostCeiling>().is_err(),
        "f64::from_str accepts NaN by name"
    );
    assert!("0".parse::<CostCeiling>().is_err());
    assert!("free".parse::<CostCeiling>().is_err());
}

#[test]
fn a_ledger_with_no_ceiling_always_admits_but_still_counts_spend() {
    let ledger = CostLedger::open(None, HarnessPricing::Publishes).unwrap();
    assert_eq!(ledger.admit(), Admission::Proceed);
    ledger.record(Some(&RunCost::Priced { usd: 50.0 }));
    assert_eq!(ledger.spent_usd(), 50.0);
    assert_eq!(ledger.admit(), Admission::Proceed);
    assert!(!ledger.exhausted());
}

#[test]
fn a_ledger_admits_runs_until_spend_reaches_the_ceiling_and_refuses_afterwards() {
    let ceiling = CostCeiling::parse(10.0).unwrap();
    let ledger = CostLedger::open(Some(ceiling), HarnessPricing::Publishes).unwrap();

    assert_eq!(ledger.admit(), Admission::Proceed);
    ledger.record(Some(&RunCost::Priced { usd: 6.0 }));
    assert_eq!(
        ledger.admit(),
        Admission::Proceed,
        "spend has not reached the ceiling yet"
    );

    ledger.record(Some(&RunCost::Priced { usd: 4.0 }));
    assert_eq!(
        ledger.admit(),
        Admission::Exhausted {
            spent_usd: 10.0,
            ceiling_usd: 10.0
        }
    );
    assert!(ledger.exhausted());
}

#[test]
fn a_ledger_that_lands_exactly_on_its_ceiling_is_exhausted_but_has_not_overspent() {
    let ledger = CostLedger::open(Some(CostCeiling::parse(10.0).unwrap()), HarnessPricing::Publishes).unwrap();
    ledger.record(Some(&RunCost::Priced { usd: 10.0 }));

    assert!(ledger.exhausted(), "it would admit nothing further");
    assert!(!ledger.overspent(), "it paid exactly what it was allowed to");
}

#[test]
fn a_ledger_a_single_run_pushed_past_its_ceiling_has_overspent() {
    let ledger = CostLedger::open(Some(CostCeiling::parse(1.0).unwrap()), HarnessPricing::Publishes).unwrap();
    ledger.record(Some(&RunCost::Priced { usd: 5.0 }));

    assert!(
        ledger.overspent(),
        "admission is checked, not reserved, so one run can pass it"
    );
}

#[test]
fn a_ledger_with_no_ceiling_can_never_overspend() {
    let ledger = CostLedger::open(None, HarnessPricing::Publishes).unwrap();
    ledger.record(Some(&RunCost::Priced { usd: 1_000.0 }));

    assert!(!ledger.overspent(), "nothing was promised, so nothing was exceeded");
}

#[test]
fn a_run_with_no_reported_cost_records_nothing() {
    let ledger = CostLedger::open(Some(CostCeiling::parse(1.0).unwrap()), HarnessPricing::Publishes).unwrap();
    ledger.record(None);
    assert_eq!(ledger.spent_usd(), 0.0);
}

#[test]
fn a_negative_or_non_finite_cost_records_nothing() {
    let ledger = CostLedger::open(None, HarnessPricing::Publishes).unwrap();
    ledger.record(Some(&RunCost::Priced { usd: -3.0 }));
    ledger.record(Some(&RunCost::Priced { usd: f64::NAN }));
    ledger.record(Some(&RunCost::Priced { usd: f64::INFINITY }));
    assert_eq!(ledger.spent_usd(), 0.0);
}

fn silent() -> HarnessPricing {
    HarnessPricing::Silent {
        harness: "codex".to_string(),
    }
}

/// A ceiling over a harness that never reports a price would sit at zero spend for
/// the life of the pass, admit every run, and leave the operator believing a limit
/// held while the bill grew.
#[test]
fn a_ceiling_nothing_could_ever_reach_is_refused() {
    let refused = CostLedger::open(Some(CostCeiling::parse(5.0).unwrap()), silent());
    assert!(refused.is_err());
    assert!(refused.unwrap_err().contains("codex"));
}

#[test]
fn a_pass_with_no_ceiling_still_runs_on_a_harness_that_reports_no_price() {
    assert!(CostLedger::open(None, silent()).is_ok());
}

/// Zero dollars spent and nobody able to say what was spent are different answers,
/// and a reader totalling spend across harnesses has to be able to tell them apart.
#[test]
fn a_pass_nobody_could_price_reports_no_total_rather_than_zero() {
    let ledger = CostLedger::open(None, silent()).unwrap();
    ledger.record(Some(&RunCost::Priced { usd: 4.0 }));
    assert_eq!(ledger.spend().usd(), None);
    assert_eq!(
        ledger.spend(),
        PassSpend::Unpriced {
            harness: "codex".to_string()
        }
    );
}

#[test]
fn a_pass_the_harness_priced_reports_what_it_came_to() {
    let ledger = CostLedger::open(None, HarnessPricing::Publishes).unwrap();
    ledger.record(Some(&RunCost::Priced { usd: 0.25 }));
    assert_eq!(ledger.spend(), PassSpend::Priced { usd: 0.25 });
    assert_eq!(ledger.spend().usd(), Some(0.25));
}

#[test]
fn a_priced_pass_that_spent_nothing_still_reports_a_total() {
    let ledger = CostLedger::open(None, HarnessPricing::Publishes).unwrap();
    assert_eq!(ledger.spend(), PassSpend::Priced { usd: 0.0 });
}

/// Reading a run of a harness that prices nothing as a run that happened to report no
/// price is what makes a cross-harness total wrong: the absent number gets added as a
/// zero and the harness comes out cheaper than the one that answered.
#[test]
fn a_run_of_a_harness_that_prices_nothing_says_so_rather_than_reporting_no_price() {
    assert_eq!(
        silent().price(None),
        Some(RunCost::Unpriced {
            harness: "codex".to_string()
        })
    );
}

#[test]
fn a_run_a_pricing_harness_did_not_price_carries_no_price_at_all() {
    assert_eq!(HarnessPricing::Publishes.price(None), None);
}

#[test]
fn a_run_a_pricing_harness_priced_carries_what_it_reported() {
    assert_eq!(
        HarnessPricing::Publishes.price(Some(0.25)),
        Some(RunCost::Priced { usd: 0.25 })
    );
}

#[test]
fn a_cap_of_zero_or_garbage_is_refused() {
    assert!("0".parse::<MaxToolCalls>().is_err());
    assert!("0".parse::<MaxTurns>().is_err());
    assert!("-1".parse::<MaxTurns>().is_err());
    assert!("many".parse::<MaxToolCalls>().is_err());
    assert_eq!("25".parse::<MaxToolCalls>().unwrap().get(), 25);
}

#[test]
fn a_run_crosses_a_cap_only_once_it_goes_past_it() {
    let caps = RunCaps {
        tool_calls: Some("2".parse().unwrap()),
        turns: Some("3".parse().unwrap()),
    };

    let crossed = |tool_calls, turns| caps.crossed(ToolCallCount::new(tool_calls), TurnCount::new(turns));
    assert_eq!(crossed(2, 3), None);
    assert_eq!(
        crossed(3, 0).map(RunCap::failure_kind),
        Some(FAILURE_KIND_TOOL_CALL_CAP)
    );
    assert_eq!(crossed(0, 4).map(RunCap::failure_kind), Some(FAILURE_KIND_TURN_CAP));
    assert_eq!(
        RunCaps::default().crossed(ToolCallCount::new(u64::MAX), TurnCount::new(u64::MAX)),
        None
    );
    assert!(RunCaps::default().is_unbounded());
}
