//! A dollar ceiling on what one pass may spend before it stops starting new runs.
//!
//! Attempts default to three draws per (case, scenario) cell, so an ordinary pass now
//! pays for three model calls where it once paid for one. Nothing before this type ever
//! asked what a pass was allowed to spend, so a pass that got the case count, the
//! scenario count, or the draw count wrong spent the difference before a report existed
//! to catch it.

use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

/// The failure kind recorded on a run the ledger refused to start.
pub const FAILURE_KIND_BUDGET: &str = "budget";

/// A ceiling on what one pass may spend, in US dollars.
///
/// Bounded to strictly positive amounts. A ceiling of zero or less admits no run at all,
/// which is not a budget, it is a way to spell `--runner` and mean nothing to happen.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct CostCeiling(f64);

impl CostCeiling {
    pub fn parse(usd: f64) -> Result<Self, String> {
        if !usd.is_finite() {
            return Err(
                "cost ceiling must be a finite dollar amount: a ceiling that cannot be compared against spend cannot admit or refuse a run"
                    .to_string(),
            );
        }
        if usd <= 0.0 {
            return Err(
                "cost ceiling must be a positive dollar amount: a pass allowed to spend nothing has nothing left to admit"
                    .to_string(),
            );
        }
        Ok(Self(usd))
    }

    pub fn usd(self) -> f64 {
        self.0
    }
}

impl fmt::Display for CostCeiling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for CostCeiling {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let usd: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not a dollar amount"))?;
        Self::parse(usd)
    }
}

/// Whether a run may start, and what it costs to say no.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Admission {
    Proceed,
    Exhausted { spent_usd: f64, ceiling_usd: f64 },
}

const MICRO_USD_PER_USD: f64 = 1_000_000.0;

/// What one pass has spent, checked before every run and updated after every one that
/// executes.
///
/// Shared across the lanes `execute_all` spawns, so the running total sits behind an
/// atomic rather than a lock: checking it or adding to it must never block a lane on
/// another lane's write. f64 has no atomic form, so the total is kept in micro-dollars,
/// small enough a unit that a real invoice still round-trips through it without losing a
/// cent.
#[derive(Debug)]
pub struct CostLedger {
    ceiling: Option<CostCeiling>,
    spent_micro_usd: AtomicU64,
}

impl CostLedger {
    pub fn new(ceiling: Option<CostCeiling>) -> Self {
        Self {
            ceiling,
            spent_micro_usd: AtomicU64::new(0),
        }
    }

    pub fn ceiling(&self) -> Option<CostCeiling> {
        self.ceiling
    }

    pub fn spent_usd(&self) -> f64 {
        self.spent_micro_usd.load(Ordering::SeqCst) as f64 / MICRO_USD_PER_USD
    }

    /// Add a run's cost to the running total.
    ///
    /// A run the runner never priced adds nothing: a missing price is not a free run, it
    /// is one nobody can charge for yet. The same holds for a price that came back
    /// negative or non-finite, which answers for a bug in a runner rather than a run
    /// that earned money back.
    pub fn record(&self, cost_usd: Option<f64>) {
        let Some(cost_usd) = cost_usd else {
            return;
        };
        if !cost_usd.is_finite() || cost_usd <= 0.0 {
            return;
        }
        let micro_usd = (cost_usd * MICRO_USD_PER_USD).round() as u64;
        self.spent_micro_usd.fetch_add(micro_usd, Ordering::SeqCst);
    }

    /// Whether a run may start.
    ///
    /// Checked, not reserved. A lane admitted here still spends whatever the run costs
    /// before the next check can see it, so a pass can overshoot by the runs already in
    /// flight when it stops. That is the price of a check cheap enough to run before
    /// every one of them, rather than one that has to coordinate lanes to answer.
    pub fn admit(&self) -> Admission {
        match self.ceiling {
            None => Admission::Proceed,
            Some(ceiling) => {
                let spent_usd = self.spent_usd();
                if spent_usd >= ceiling.usd() {
                    Admission::Exhausted {
                        spent_usd,
                        ceiling_usd: ceiling.usd(),
                    }
                } else {
                    Admission::Proceed
                }
            }
        }
    }

    /// Whether the pass has, as of now, spent its way past the ceiling.
    pub fn exhausted(&self) -> bool {
        matches!(self.admit(), Admission::Exhausted { .. })
    }

    /// Whether the pass spent strictly more than it was allowed to.
    ///
    /// Distinct from `exhausted`, which is true the moment spend reaches the ceiling.
    /// A pass whose last run lands exactly on the ceiling is exhausted, in that it would
    /// admit nothing further, but it has not overspent: every run it was asked for ran,
    /// and it paid what it said it would. Only this answers "did the operator get less,
    /// or pay more, than the ceiling promised".
    pub fn overspent(&self) -> bool {
        match self.ceiling {
            None => false,
            Some(ceiling) => self.spent_usd() > ceiling.usd(),
        }
    }
}

#[cfg(test)]
mod tests {
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
        let ledger = CostLedger::new(None);
        assert_eq!(ledger.admit(), Admission::Proceed);
        ledger.record(Some(50.0));
        assert_eq!(ledger.spent_usd(), 50.0);
        assert_eq!(ledger.admit(), Admission::Proceed);
        assert!(!ledger.exhausted());
    }

    #[test]
    fn a_ledger_admits_runs_until_spend_reaches_the_ceiling_and_refuses_afterwards() {
        let ceiling = CostCeiling::parse(10.0).unwrap();
        let ledger = CostLedger::new(Some(ceiling));

        assert_eq!(ledger.admit(), Admission::Proceed);
        ledger.record(Some(6.0));
        assert_eq!(
            ledger.admit(),
            Admission::Proceed,
            "spend has not reached the ceiling yet"
        );

        ledger.record(Some(4.0));
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
        let ledger = CostLedger::new(Some(CostCeiling::parse(10.0).unwrap()));
        ledger.record(Some(10.0));

        assert!(ledger.exhausted(), "it would admit nothing further");
        assert!(!ledger.overspent(), "it paid exactly what it was allowed to");
    }

    #[test]
    fn a_ledger_a_single_run_pushed_past_its_ceiling_has_overspent() {
        let ledger = CostLedger::new(Some(CostCeiling::parse(1.0).unwrap()));
        ledger.record(Some(5.0));

        assert!(
            ledger.overspent(),
            "admission is checked, not reserved, so one run can pass it"
        );
    }

    #[test]
    fn a_ledger_with_no_ceiling_can_never_overspend() {
        let ledger = CostLedger::new(None);
        ledger.record(Some(1_000.0));

        assert!(!ledger.overspent(), "nothing was promised, so nothing was exceeded");
    }

    #[test]
    fn a_run_with_no_reported_cost_records_nothing() {
        let ledger = CostLedger::new(Some(CostCeiling::parse(1.0).unwrap()));
        ledger.record(None);
        assert_eq!(ledger.spent_usd(), 0.0);
    }

    #[test]
    fn a_negative_or_non_finite_cost_records_nothing() {
        let ledger = CostLedger::new(None);
        ledger.record(Some(-3.0));
        ledger.record(Some(f64::NAN));
        ledger.record(Some(f64::INFINITY));
        assert_eq!(ledger.spent_usd(), 0.0);
    }
}
