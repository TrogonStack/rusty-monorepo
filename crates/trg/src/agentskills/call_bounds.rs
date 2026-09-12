//! How many times a case expects a tool to be called.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The inclusive range of call counts a grader accepts.
///
/// A skill that fires on a prompt it was never meant to answer is as much a defect as
/// one that never fires, and a lower bound alone cannot say so: every count satisfies
/// it. An upper bound is what makes "this tool must not be reached for" expressible,
/// and `0..=0` is how it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallBounds {
    min: usize,
    max: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
struct DeclaredCallBounds {
    #[serde(default = "once")]
    #[schemars(range(min = 0))]
    min_calls: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0))]
    max_calls: Option<usize>,
}

fn once() -> usize {
    1
}

impl CallBounds {
    pub const fn at_least_once() -> Self {
        Self { min: 1, max: None }
    }

    pub const fn never() -> Self {
        Self { min: 0, max: Some(0) }
    }

    /// Refuses a range nothing can fall outside of, since a check that cannot fail
    /// reads in a report exactly like one that held.
    pub fn parse(min: usize, max: Option<usize>) -> Result<Self, String> {
        match max {
            Some(max) if max < min => Err(format!(
                "max_calls {max} is below min_calls {min}, so no number of calls satisfies it"
            )),
            None if min == 0 => Err(
                "min_calls 0 without max_calls accepts every run; give max_calls to say a tool must not be reached for"
                    .to_string(),
            ),
            _ => Ok(Self { min, max }),
        }
    }

    pub fn admits(&self, calls: usize) -> bool {
        calls >= self.min && self.max.is_none_or(|max| calls <= max)
    }

    pub fn min(&self) -> usize {
        self.min
    }

    pub fn max(&self) -> Option<usize> {
        self.max
    }
}

impl Default for CallBounds {
    fn default() -> Self {
        Self::at_least_once()
    }
}

impl fmt::Display for CallBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.min, self.max) {
            (0, Some(0)) => f.write_str("never"),
            (min, Some(max)) if min == max => write!(f, "exactly {min} time(s)"),
            (0, Some(max)) => write!(f, "at most {max} time(s)"),
            (1, None) => f.write_str("at least once"),
            (min, None) => write!(f, "at least {min} time(s)"),
            (min, Some(max)) => write!(f, "between {min} and {max} time(s)"),
        }
    }
}

impl Serialize for CallBounds {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DeclaredCallBounds {
            min_calls: self.min,
            max_calls: self.max,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CallBounds {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let declared = DeclaredCallBounds::deserialize(deserializer)?;
        Self::parse(declared.min_calls, declared.max_calls).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for CallBounds {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        DeclaredCallBounds::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        DeclaredCallBounds::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_a_case_forbids_admits_no_calls_at_all() {
        let bounds = CallBounds::never();
        assert!(bounds.admits(0));
        assert!(!bounds.admits(1));
        assert_eq!(bounds.to_string(), "never");
    }

    #[test]
    fn the_default_asks_for_one_call_and_accepts_more() {
        let bounds = CallBounds::default();
        assert!(!bounds.admits(0));
        assert!(bounds.admits(1));
        assert!(bounds.admits(9));
    }

    #[test]
    fn a_range_nothing_falls_outside_of_is_refused() {
        assert!(CallBounds::parse(0, None).is_err());
        assert!(CallBounds::parse(2, Some(1)).is_err());
        assert!(CallBounds::parse(0, Some(0)).is_ok());
        assert!(CallBounds::parse(2, Some(2)).is_ok());
    }

    #[test]
    fn bounds_round_trip_through_the_fields_a_case_declares() {
        let declared = serde_json::json!({ "min_calls": 0, "max_calls": 0 });
        let bounds: CallBounds = serde_json::from_value(declared.clone()).unwrap();
        assert_eq!(bounds, CallBounds::never());
        assert_eq!(serde_json::to_value(bounds).unwrap(), declared);
    }

    #[test]
    fn a_case_that_says_nothing_about_counts_asks_for_one_call() {
        let bounds: CallBounds = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(bounds, CallBounds::at_least_once());
    }
}
