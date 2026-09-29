//! Whether the with_skill arm has any room left to show an improvement.
//!
//! A suite whose with_skill assertions already clear a high floor cannot register a
//! change as an improvement no matter how good the change is: there is nothing left
//! above the floor for a better skill to reach. The same Wilson interval the
//! keep-or-revert verdict already trusts is the one that says a suite has stopped
//! measuring anything, so this reuses it rather than inventing a second statistic.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::proportion::Proportion;

/// What a headroom threshold defaults to unless the operator says otherwise.
///
/// A suite whose worst-case (95% lower-bound) with_skill pass rate has already cleared
/// nine in ten leaves at most one in ten of room for a change to show up in, and more
/// attempts only tighten that floor further. A suite that trips this warning at 0.9 is
/// not going to grow room back on its own; it needs harder cases, not more draws.
const DEFAULT_HEADROOM_THRESHOLD: f64 = 0.9;

/// The Wilson lower bound a with_skill arm is treated as saturated at or above.
///
/// A proportion in `(0, 1]`: zero would call every arm saturated before a single case
/// runs, so it is refused rather than accepted as an edge case.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct HeadroomThreshold(f64);

impl HeadroomThreshold {
    pub fn parse(value: f64) -> Result<Self, String> {
        if !value.is_finite() || value <= 0.0 || value > 1.0 {
            return Err(format!("a headroom threshold of {value} is not a proportion in (0, 1]"));
        }
        Ok(Self(value))
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

impl Default for HeadroomThreshold {
    fn default() -> Self {
        Self(DEFAULT_HEADROOM_THRESHOLD)
    }
}

impl<'de> Deserialize<'de> for HeadroomThreshold {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = f64::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for HeadroomThreshold {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HeadroomThreshold".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
    /// schema looser than its own parser lets `eval verify --mode strict` call a document
    /// conformant that its own builder would have refused to produce.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "The with_skill arm's Wilson lower bound is treated as saturated at or above this proportion. Greater than 0 and at most 1.",
            "type": "number",
            "exclusiveMinimum": 0.0,
            "maximum": 1.0
        })
    }
}

impl fmt::Display for HeadroomThreshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for HeadroomThreshold {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not a headroom threshold"))?;
        Self::parse(parsed)
    }
}

/// A with_skill arm that has used up its room to show improvement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeadroomWarning {
    /// The Wilson lower bound that met or crossed `threshold`.
    pub lower_bound: f64,
    pub threshold: HeadroomThreshold,
    /// Cases whose with_skill attempts passed every scored assertion across every draw.
    ///
    /// Named by id rather than counted, because `benchmark.json` and
    /// `iteration-summary.json` are the author's own measurement surfaces and already
    /// list every case id elsewhere in the same document; only the improvement bundle
    /// withholds test-split ids from a wider audience.
    pub saturated_case_ids: Vec<String>,
}

/// A warning when the with_skill arm's pass-rate floor has reached `threshold`, naming
/// the cases that individually passed everything drawn of them.
///
/// Returns nothing when there is no scored assertion to build a proportion from, since an
/// arm nobody scored has not demonstrated saturation any more than it has demonstrated
/// improvement.
pub fn evaluate_headroom(
    threshold: HeadroomThreshold,
    assertions: Option<Proportion>,
    mut saturated_case_ids: Vec<String>,
) -> Option<HeadroomWarning> {
    let assertions = assertions?;
    let lower_bound = assertions.interval().low();
    if lower_bound < threshold.value() {
        return None;
    }
    saturated_case_ids.sort();
    saturated_case_ids.dedup();
    Some(HeadroomWarning {
        lower_bound,
        threshold,
        saturated_case_ids,
    })
}

/// A one-line, human-readable rendering of a headroom warning, prefixed with `scope` so
/// the same line reads correctly whether it names the whole document or one split.
pub fn describe_headroom_warning(scope: &str, warning: &HeadroomWarning) -> String {
    let cases = if warning.saturated_case_ids.is_empty() {
        "none individually saturated".to_string()
    } else {
        warning.saturated_case_ids.join(", ")
    };
    format!(
        "WARN: {scope} with_skill has no headroom left: pass-rate floor {:.3} is at or above the {} threshold. Saturated cases: {cases}",
        warning.lower_bound, warning.threshold
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threshold_outside_zero_to_one_is_refused() {
        assert!(HeadroomThreshold::parse(0.0).is_err());
        assert!(HeadroomThreshold::parse(-0.1).is_err());
        assert!(HeadroomThreshold::parse(1.1).is_err());
        assert!(HeadroomThreshold::parse(f64::NAN).is_err());
        assert!(HeadroomThreshold::parse(f64::INFINITY).is_err());
    }

    #[test]
    fn a_threshold_of_exactly_one_is_admitted() {
        assert_eq!(HeadroomThreshold::parse(1.0).unwrap().value(), 1.0);
    }

    #[test]
    fn the_default_threshold_is_nine_in_ten() {
        assert_eq!(HeadroomThreshold::default().value(), 0.9);
    }

    #[test]
    fn a_threshold_is_parsed_from_the_command_line_form() {
        assert_eq!("0.9".parse::<HeadroomThreshold>().unwrap().value(), 0.9);
        assert!("0".parse::<HeadroomThreshold>().is_err());
        assert!("some".parse::<HeadroomThreshold>().is_err());
    }

    #[test]
    fn an_arm_with_no_scored_assertions_cannot_be_saturated() {
        assert!(evaluate_headroom(HeadroomThreshold::default(), None, vec![]).is_none());
    }

    #[test]
    fn an_arm_below_the_floor_is_not_saturated() {
        let assertions = Proportion::observed(8, 2);
        assert!(evaluate_headroom(HeadroomThreshold::default(), assertions, vec![]).is_none());
    }

    #[test]
    fn an_arm_at_or_above_the_floor_is_saturated_and_names_its_cases() {
        let assertions = Proportion::observed(40, 0);
        let warning = evaluate_headroom(
            HeadroomThreshold::default(),
            assertions,
            vec!["case-b".to_string(), "case-a".to_string(), "case-a".to_string()],
        )
        .expect("a floor of 40/40 clears the default threshold");

        assert!(warning.lower_bound >= HeadroomThreshold::default().value());
        assert_eq!(
            warning.saturated_case_ids,
            vec!["case-a".to_string(), "case-b".to_string()]
        );
    }
}
