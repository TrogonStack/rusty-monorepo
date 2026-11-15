//! What "unusual" means for a graded pass.
//!
//! A pass emits more than one summary of the same runs, and a reader who sees the same
//! run called an outlier in one and ordinary in the other has no way to decide which
//! artifact to believe. The definitions live here once so the artifacts cannot disagree.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{de, Deserialize, Deserializer, Serialize};

use super::report::ScenarioKind;

/// How many samples a group needs before its spread says anything.
///
/// A group of two cannot say which of the pair was unusual, and a group of three draws
/// the band it judges a sample against out of that sample itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SampleFloor(usize);

impl SampleFloor {
    /// What a dispersion verdict asks for.
    pub const fn dispersion() -> Self {
        Self(4)
    }

    pub fn parse(samples: usize) -> Result<Self, String> {
        if samples < 2 {
            return Err(format!(
                "a floor of {samples} accepts a group with no spread at all, which reports every draw as typical"
            ));
        }
        Ok(Self(samples))
    }

    pub fn count(self) -> usize {
        self.0
    }

    pub fn admits(self, observed: usize) -> bool {
        observed >= self.0
    }
}

impl Default for SampleFloor {
    fn default() -> Self {
        Self::dispersion()
    }
}

/// Where a group of samples sits and how far from that a sample may sit.
///
/// The centre is the median and the spread is the median absolute deviation, not the
/// mean and the standard deviation. Both of the latter are moved by the very sample
/// being looked for: one extreme draw widens the band meant to catch it until the draw
/// falls inside, so an outlier large enough hides itself.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Dispersion {
    centre: f64,
    spread: f64,
}

impl Dispersion {
    /// How many spreads from the centre a sample is still explained by its group.
    pub const ADMITTED_SPREADS: f64 = 3.0;

    /// Refuses a spread of zero, which happens when more than half of a group holds the
    /// same value. Every value that is not the modal one then sits infinitely far out,
    /// so the whole tail would be published as unusual when nothing about it is.
    pub fn parse(centre: f64, spread: f64) -> Result<Self, String> {
        if !centre.is_finite() || !spread.is_finite() {
            return Err("a dispersion needs a finite centre and spread".to_string());
        }
        if spread <= 0.0 {
            return Err(format!(
                "spread {spread} leaves no room around the centre, so every value that is not exactly {centre} reads as unusual"
            ));
        }
        Ok(Self { centre, spread })
    }

    /// The dispersion of a group, or nothing when the group cannot describe itself.
    pub fn measure(values: &[u64]) -> Option<Self> {
        let mut readings: Vec<f64> = values.iter().map(|value| *value as f64).collect();
        let centre = median(&mut readings)?;
        let mut deviations: Vec<f64> = readings.iter().map(|value| (value - centre).abs()).collect();
        let spread = median(&mut deviations)?;
        Self::parse(centre, spread).ok()
    }

    pub fn centre(self) -> f64 {
        self.centre
    }

    pub fn spread(self) -> f64 {
        self.spread
    }

    pub fn cutoff(self) -> f64 {
        self.centre + Self::ADMITTED_SPREADS * self.spread
    }

    pub fn admits(self, value: u64) -> bool {
        value as f64 <= self.cutoff()
    }
}

pub(crate) fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

/// One metric reading together with the run it was taken from.
///
/// The reading on its own cannot be published: an operator told that something took an
/// unusual amount of time has nowhere to go unless the run is named alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricSample<Origin> {
    scenario: ScenarioKind,
    origin: Origin,
    value: u64,
}

impl<Origin> MetricSample<Origin> {
    pub fn new(scenario: ScenarioKind, origin: Origin, value: u64) -> Self {
        Self {
            scenario,
            origin,
            value,
        }
    }

    pub fn scenario(&self) -> ScenarioKind {
        self.scenario
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    pub fn value(&self) -> u64 {
        self.value
    }

    pub fn into_parts(self) -> (ScenarioKind, Origin, u64) {
        (self.scenario, self.origin, self.value)
    }
}

/// A sample its own group does not account for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outlier<Origin> {
    sample: MetricSample<Origin>,
    dispersion: Dispersion,
}

impl<Origin> Outlier<Origin> {
    pub fn sample(&self) -> &MetricSample<Origin> {
        &self.sample
    }

    pub fn dispersion(&self) -> Dispersion {
        self.dispersion
    }

    pub fn into_parts(self) -> (MetricSample<Origin>, Dispersion) {
        (self.sample, self.dispersion)
    }
}

/// The samples no group explains, grouped per scenario.
///
/// Per scenario rather than pooled: the arms of a comparison pay different costs by
/// construction, so a pool mixes two distributions and reports the whole of the slower
/// arm as a field of outliers.
pub fn outliers<Origin>(samples: Vec<MetricSample<Origin>>) -> Vec<Outlier<Origin>> {
    let floor = SampleFloor::dispersion();
    let mut by_scenario: BTreeMap<ScenarioKind, Vec<MetricSample<Origin>>> = BTreeMap::new();
    for sample in samples {
        by_scenario.entry(sample.scenario()).or_default().push(sample);
    }

    let mut found = Vec::new();
    for group in by_scenario.into_values() {
        if !floor.admits(group.len()) {
            continue;
        }
        let values: Vec<u64> = group.iter().map(MetricSample::value).collect();
        let Some(dispersion) = Dispersion::measure(&values) else {
            continue;
        };
        for sample in group {
            if !dispersion.admits(sample.value()) {
                found.push(Outlier { sample, dispersion });
            }
        }
    }

    found
}

/// Which assertion, on which case, under which arm.
///
/// The case is part of the key because two cases are free to word an assertion the same
/// way. Keyed on the text alone, a check that always holds for one case and always fails
/// for another is published as a single check that flips.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssertionKey {
    eval_case_id: String,
    scenario: ScenarioKind,
    assertion: String,
}

impl AssertionKey {
    /// Refuses an assertion with nothing written on it, which cannot be told apart from
    /// any other unnamed assertion on the same case.
    pub fn parse(eval_case_id: &str, scenario: ScenarioKind, assertion: &str) -> Result<Self, String> {
        let assertion = assertion.trim();
        if assertion.is_empty() {
            return Err("an assertion with no text names no check".to_string());
        }
        Ok(Self {
            eval_case_id: eval_case_id.to_string(),
            scenario,
            assertion: assertion.to_string(),
        })
    }

    pub fn eval_case_id(&self) -> &str {
        &self.eval_case_id
    }

    pub fn scenario(&self) -> ScenarioKind {
        self.scenario
    }

    pub fn assertion(&self) -> &str {
        &self.assertion
    }
}

/// How the attempts behind one key came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptTally {
    attempts: u32,
    pass_count: u32,
}

impl AttemptTally {
    pub fn parse(attempts: u32, pass_count: u32) -> Result<Self, String> {
        if attempts == 0 {
            return Err("a key nobody attempted has no outcome to tally".to_string());
        }
        if pass_count > attempts {
            return Err(format!(
                "{pass_count} passes across {attempts} attempts counts outcomes that were never drawn"
            ));
        }
        Ok(Self { attempts, pass_count })
    }

    pub fn attempts(self) -> u32 {
        self.attempts
    }

    pub fn pass_count(self) -> u32 {
        self.pass_count
    }

    pub fn fail_count(self) -> u32 {
        self.attempts - self.pass_count
    }

    /// Whether the attempts disagreed with each other, which is the whole of what flaky
    /// means: a check that always fails is broken, not unstable.
    pub fn is_split(self) -> bool {
        self.pass_count > 0 && self.pass_count < self.attempts
    }

    pub fn flakiness_ratio(self) -> FlakinessRatio {
        FlakinessRatio::parse(f64::from(self.fail_count()) / f64::from(self.attempts))
            .expect("a share of the attempts drawn lies within 0.0..=1.0")
    }
}

/// The share of a key's attempts that did not pass, in `0.0..=1.0`.
///
/// A share rather than a count, because one flip out of twelve and six out of twelve are
/// the same verdict and very different findings.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, JsonSchema)]
#[schemars(schema_with = "flakiness_ratio_schema")]
pub struct FlakinessRatio(f64);

fn flakiness_ratio_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "number",
        "minimum": 0.0,
        "maximum": 1.0
    })
}

impl FlakinessRatio {
    pub fn parse(ratio: f64) -> Result<Self, String> {
        if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
            return Err(format!("{ratio} is not a share of the attempts observed"));
        }
        Ok(Self(ratio))
    }

    pub fn value(self) -> f64 {
        self.0
    }

    pub fn percent(self) -> f64 {
        self.0 * 100.0
    }
}

impl<'de> Deserialize<'de> for FlakinessRatio {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ratio = f64::deserialize(deserializer)?;
        Self::parse(ratio).map_err(de::Error::custom)
    }
}

/// An assertion whose attempts under one key did not agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlakyAssertion {
    key: AssertionKey,
    tally: AttemptTally,
}

impl FlakyAssertion {
    pub fn key(&self) -> &AssertionKey {
        &self.key
    }

    pub fn tally(&self) -> AttemptTally {
        self.tally
    }

    pub fn into_record(self) -> FlakyAssertionRecord {
        FlakyAssertionRecord {
            eval_case_id: self.key.eval_case_id,
            scenario_id: self.key.scenario.as_str().to_string(),
            assertion: self.key.assertion,
            attempts: self.tally.attempts(),
            pass_count: self.tally.pass_count(),
            flakiness_ratio: self.tally.flakiness_ratio(),
        }
    }
}

/// What every artifact publishes about a flaky assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FlakyAssertionRecord {
    pub eval_case_id: String,
    pub scenario_id: String,
    pub assertion: String,
    pub attempts: u32,
    pub pass_count: u32,
    pub flakiness_ratio: FlakinessRatio,
}

/// Every assertion outcome a pass observed, waiting to be asked which keys disagreed.
#[derive(Debug, Clone, Default)]
pub struct FlakinessLedger {
    observations: BTreeMap<AssertionKey, BTreeMap<u32, bool>>,
}

impl FlakinessLedger {
    pub fn observe(&mut self, key: AssertionKey, attempt: u32, passed: bool) {
        self.observations.entry(key).or_default().insert(attempt, passed);
    }

    pub fn flaky_assertions(&self) -> Vec<FlakyAssertion> {
        self.observations
            .iter()
            .filter_map(|(key, attempts)| {
                let pass_count = attempts.values().filter(|passed| **passed).count() as u32;
                let tally = AttemptTally::parse(attempts.len() as u32, pass_count).ok()?;
                tally.is_split().then(|| FlakyAssertion {
                    key: key.clone(),
                    tally,
                })
            })
            .collect()
    }

    pub fn flaky_records(&self) -> Vec<FlakyAssertionRecord> {
        self.flaky_assertions()
            .into_iter()
            .map(FlakyAssertion::into_record)
            .collect()
    }
}

#[cfg(test)]
#[path = "dispersion_tests.rs"]
mod tests;
