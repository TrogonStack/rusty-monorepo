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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

/// Whether the harness running a pass reports what its runs cost.
///
/// Not every harness publishes a price. One that does not still spends real money, so a
/// pass of its runs cannot be totalled and cannot be bounded, and this is what says which
/// of the two kinds a pass is dealing with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessPricing {
    /// Every completed run comes back with what it cost.
    Publishes,
    /// No run ever comes back with a price. Named, so a report can say which harness.
    Silent { harness: String },
}

impl HarnessPricing {
    /// The one place a harness's reported price becomes what a run's record says about
    /// it, so a harness that prices nothing cannot be written down as a harness that
    /// happened to report nothing this time.
    pub fn price(&self, reported_usd: Option<f64>) -> Option<RunCost> {
        match self {
            Self::Publishes => reported_usd.map(|usd| RunCost::Priced { usd }),
            Self::Silent { harness } => Some(RunCost::Unpriced {
                harness: harness.clone(),
            }),
        }
    }
}

/// What one run cost, or why nobody can say.
///
/// A run whose harness prices nothing and a run whose harness prices everything but did
/// not price this one are two different facts, and a bare absent number carries neither.
/// A reader totalling an absent price as zero reports a harness as cheap when what it
/// actually did was decline to answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunCost {
    /// The harness priced this run, and this is what it came to.
    Priced {
        #[schemars(range(min = 0.0))]
        usd: f64,
    },
    /// The harness prices no run at all, so this one has no cost to report and is not a
    /// run that cost nothing. Named, so a report totalling a mixed history can say which
    /// harness it cannot total.
    Unpriced { harness: String },
}

impl RunCost {
    pub fn usd(&self) -> Option<f64> {
        match self {
            Self::Priced { usd } => Some(*usd),
            Self::Unpriced { .. } => None,
        }
    }

    pub fn is_unpriced(&self) -> bool {
        matches!(self, Self::Unpriced { .. })
    }
}

/// What a pass spent, or why nobody can say.
///
/// A harness that publishes no price leaves a ledger holding zero, and a zero in a spend
/// field reads as a cheap pass rather than an unanswered question. Only a total someone
/// actually priced is ever written as a number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PassSpend {
    /// The harness priced its runs, and this is what they came to.
    Priced {
        #[schemars(range(min = 0.0))]
        usd: f64,
    },
    /// The harness reports no price for a run, so this pass has no total to report.
    Unpriced { harness: String },
}

impl PassSpend {
    pub fn usd(&self) -> Option<f64> {
        match self {
            Self::Priced { usd } => Some(*usd),
            Self::Unpriced { .. } => None,
        }
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
    pricing: HarnessPricing,
    spent_micro_usd: AtomicU64,
}

impl CostLedger {
    /// Open a ledger for a pass, refusing a ceiling nothing could ever reach.
    ///
    /// A ceiling over a harness that publishes no price is worse than no ceiling at all:
    /// spend stays at zero however long the pass runs, so the ceiling admits every run
    /// and the operator believes they are bounded while the bill grows.
    pub fn open(ceiling: Option<CostCeiling>, pricing: HarnessPricing) -> Result<Self, String> {
        if let (Some(_), HarnessPricing::Silent { harness }) = (ceiling, &pricing) {
            return Err(format!(
                "--max-cost-usd cannot bound a pass run by '{harness}': it reports no price for a run, so spend would stay at zero, every run would be admitted, and the ceiling would promise a limit it could not hold"
            ));
        }
        Ok(Self {
            ceiling,
            pricing,
            spent_micro_usd: AtomicU64::new(0),
        })
    }

    /// What the pass spent, as a report may state it.
    pub fn spend(&self) -> PassSpend {
        match &self.pricing {
            HarnessPricing::Publishes => PassSpend::Priced { usd: self.spent_usd() },
            HarnessPricing::Silent { harness } => PassSpend::Unpriced {
                harness: harness.clone(),
            },
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
    pub fn record(&self, cost: Option<&RunCost>) {
        let Some(cost_usd) = cost.and_then(RunCost::usd) else {
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
}
