//! A dollar ceiling on what one pass may spend before it stops starting new runs.
//!
//! Attempts default to three draws per (case, scenario) cell, so an ordinary pass now
//! pays for three model calls where it once paid for one. Nothing before this type ever
//! asked what a pass was allowed to spend, so a pass that got the case count, the
//! scenario count, or the draw count wrong spent the difference before a report existed
//! to catch it.

mod constants;
use constants::MICRO_USD_PER_USD;
pub use constants::{FAILURE_KIND_BUDGET, FAILURE_KIND_TOOL_CALL_CAP, FAILURE_KIND_TURN_CAP};
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::transcript_metrics::{ToolCallCount, TurnCount};

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

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The established command-line parser contract returns a field-specific String validation message instead of a source-bearing error"
        )
    )]
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

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "The established command-line cap parser returns a field-specific String validation message instead of a source-bearing error"
    )
)]
fn parse_cap(value: &str, what: &str) -> Result<NonZeroU32, String> {
    let count: u32 = value
        .trim()
        .parse()
        .map_err(|_| format!("'{value}' is not a whole number of {what}"))?;
    NonZeroU32::new(count).ok_or_else(|| {
        format!("a cap of zero {what} stops every run before it starts, which is not a cap, it is a way to run nothing")
    })
}

/// The most tool calls one run may make before it is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaxToolCalls(NonZeroU32);

impl MaxToolCalls {
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

impl FromStr for MaxToolCalls {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_cap(value, "tool calls").map(Self)
    }
}

impl fmt::Display for MaxToolCalls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The most model turns one run may take before it is stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaxTurns(NonZeroU32);

impl MaxTurns {
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

impl FromStr for MaxTurns {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_cap(value, "turns").map(Self)
    }
}

impl fmt::Display for MaxTurns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What one run may spend in tool calls and turns, each unbounded when absent.
///
/// The pass-level [`CostCeiling`] only refuses runs it has not started yet, so a single
/// run that loops on a tool or floods its context spends without limit until the clock
/// ends it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<MaxToolCalls>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<MaxTurns>,
}

impl RunCaps {
    pub fn is_unbounded(&self) -> bool {
        self.tool_calls.is_none() && self.turns.is_none()
    }

    /// The cap a run that has made `tool_calls` tool calls over `turns` turns has gone past.
    pub fn crossed(self, tool_calls: ToolCallCount, turns: TurnCount) -> Option<RunCap> {
        if let Some(cap) = self.tool_calls.filter(|cap| tool_calls.get() > u64::from(cap.get())) {
            return Some(RunCap::ToolCalls(cap));
        }
        self.turns
            .filter(|cap| turns.get() > u64::from(cap.get()))
            .map(RunCap::Turns)
    }
}

/// Which of a run's caps stopped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCap {
    ToolCalls(MaxToolCalls),
    Turns(MaxTurns),
}

impl RunCap {
    pub fn failure_kind(self) -> &'static str {
        match self {
            Self::ToolCalls(_) => FAILURE_KIND_TOOL_CALL_CAP,
            Self::Turns(_) => FAILURE_KIND_TURN_CAP,
        }
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
