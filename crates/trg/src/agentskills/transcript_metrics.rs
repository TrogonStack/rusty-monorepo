//! What a run's transcript says about how the harness spent its turns and tool calls.
//!
//! A run's token total says a run was expensive, not why. A single tool result large
//! enough to fill a turn, or a search the harness kept repeating after another tool had
//! already answered, only shows up once each tool is counted on its own.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many model turns a run took, as its harness's stream reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TurnCount(u64);

impl TurnCount {
    pub fn new(turns: u64) -> Self {
        Self(turns)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TurnCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How many tool calls a run, or one tool within it, made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolCallCount(u64);

impl ToolCallCount {
    pub fn new(calls: u64) -> Self {
        Self(calls)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    fn increment(&mut self) {
        self.0 += 1;
    }
}

impl std::ops::Add for ToolCallCount {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl fmt::Display for ToolCallCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The size of one tool result as the harness handed it back to the model, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ResultBytes(u64);

impl ResultBytes {
    pub fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ResultBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How many runs a tally covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RunCount(u64);

impl RunCount {
    pub fn new(runs: u64) -> Self {
        Self(runs)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn of(runs: usize) -> Self {
        Self(u64::try_from(runs).unwrap_or(u64::MAX))
    }

    fn increment(&mut self) {
        self.0 += 1;
    }
}

impl fmt::Display for RunCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One tool call as a transcript recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedToolCall<'a> {
    pub tool: &'a str,
    pub arguments: Option<&'a str>,
    pub result: Option<&'a str>,
}

/// How a run used one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolUsage {
    pub tool: String,
    pub calls: ToolCallCount,
    /// Calls with the same arguments as an earlier call to this tool in the same run.
    pub repeated_calls: ToolCallCount,
    /// Absent when no call to this tool reported a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub largest_result_bytes: Option<ResultBytes>,
}

/// Turn and tool counts read off one run's transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptMetrics {
    pub turns: TurnCount,
    pub tool_calls: ToolCallCount,
    /// Calls with the same tool and arguments as an earlier call in the same run.
    pub repeated_tool_calls: ToolCallCount,
    /// The most input tokens any single turn sent, cache reads and writes included.
    /// Absent for a harness that reports usage only for the whole run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_turn_input_tokens: Option<u64>,
    /// Ordered by tool name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolUsage>,
}

impl TranscriptMetrics {
    pub fn measure<'a>(
        turns: TurnCount,
        peak_turn_input_tokens: Option<u64>,
        calls: impl IntoIterator<Item = ObservedToolCall<'a>>,
    ) -> Self {
        let mut tools: BTreeMap<&str, ToolUsage> = BTreeMap::new();
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        let mut tool_calls = ToolCallCount::default();
        let mut repeated_tool_calls = ToolCallCount::default();
        for call in calls {
            tool_calls.increment();
            let usage = tools.entry(call.tool).or_insert_with(|| ToolUsage {
                tool: call.tool.to_string(),
                calls: ToolCallCount::default(),
                repeated_calls: ToolCallCount::default(),
                largest_result_bytes: None,
            });
            usage.calls.increment();
            if let Some(arguments) = call.arguments {
                if !seen.insert((call.tool, arguments)) {
                    usage.repeated_calls.increment();
                    repeated_tool_calls.increment();
                }
            }
            if let Some(result) = call.result {
                let bytes = ResultBytes::new(u64::try_from(result.len()).unwrap_or(u64::MAX));
                usage.largest_result_bytes = usage.largest_result_bytes.max(Some(bytes));
            }
        }
        Self {
            turns,
            tool_calls,
            repeated_tool_calls,
            peak_turn_input_tokens,
            tools: tools.into_values().collect(),
        }
    }
}

/// How every run in a set used one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolTally {
    pub tool: String,
    /// Runs that called this tool at least once.
    pub runs: RunCount,
    pub calls: ToolCallCount,
    pub repeated_calls: ToolCallCount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub largest_result_bytes: Option<ResultBytes>,
}

/// Every tool any of `runs` called, ordered by tool name.
pub fn tally_tools<'a>(runs: impl IntoIterator<Item = &'a TranscriptMetrics>) -> Vec<ToolTally> {
    let mut tallies: BTreeMap<&str, ToolTally> = BTreeMap::new();
    for run in runs {
        for usage in &run.tools {
            let tally = tallies.entry(usage.tool.as_str()).or_insert_with(|| ToolTally {
                tool: usage.tool.clone(),
                runs: RunCount::default(),
                calls: ToolCallCount::default(),
                repeated_calls: ToolCallCount::default(),
                largest_result_bytes: None,
            });
            tally.runs.increment();
            tally.calls = tally.calls + usage.calls;
            tally.repeated_calls = tally.repeated_calls + usage.repeated_calls;
            tally.largest_result_bytes = tally.largest_result_bytes.max(usage.largest_result_bytes);
        }
    }
    tallies.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call<'a>(tool: &'a str, arguments: Option<&'a str>, result: Option<&'a str>) -> ObservedToolCall<'a> {
        ObservedToolCall {
            tool,
            arguments,
            result,
        }
    }

    #[test]
    fn counts_calls_per_tool_and_keeps_the_largest_result() {
        let metrics = TranscriptMetrics::measure(
            TurnCount::new(3),
            Some(1200),
            [
                call("Read", Some(r#"{"path":"a"}"#), Some("12345")),
                call("Grep", Some(r#"{"q":"x"}"#), Some("[]")),
                call("Read", Some(r#"{"path":"b"}"#), Some("123456789")),
            ],
        );

        assert_eq!(metrics.turns, TurnCount::new(3));
        assert_eq!(metrics.tool_calls, ToolCallCount::new(3));
        assert_eq!(metrics.repeated_tool_calls, ToolCallCount::new(0));
        assert_eq!(metrics.peak_turn_input_tokens, Some(1200));
        let names: Vec<&str> = metrics.tools.iter().map(|tool| tool.tool.as_str()).collect();
        assert_eq!(names, ["Grep", "Read"]);
        assert_eq!(metrics.tools[1].calls, ToolCallCount::new(2));
        assert_eq!(metrics.tools[1].largest_result_bytes, Some(ResultBytes::new(9)));
    }

    #[test]
    fn a_repeated_call_is_the_same_tool_with_the_same_arguments() {
        let metrics = TranscriptMetrics::measure(
            TurnCount::new(4),
            None,
            [
                call("Grep", Some(r#"{"q":"x"}"#), Some("[]")),
                call("Grep", Some(r#"{"q":"x"}"#), Some("[]")),
                call("Glob", Some(r#"{"q":"x"}"#), None),
                call("Grep", Some(r#"{"q":"y"}"#), None),
                call("Grep", None, None),
                call("Grep", None, None),
            ],
        );

        assert_eq!(metrics.repeated_tool_calls, ToolCallCount::new(1));
        let grep = metrics.tools.iter().find(|tool| tool.tool == "Grep").unwrap();
        assert_eq!(grep.repeated_calls, ToolCallCount::new(1));
        let glob = metrics.tools.iter().find(|tool| tool.tool == "Glob").unwrap();
        assert_eq!(glob.largest_result_bytes, None);
    }

    #[test]
    fn tallies_tools_across_runs() {
        let first = TranscriptMetrics::measure(
            TurnCount::new(1),
            None,
            [call("Read", Some("a"), Some("12")), call("Read", Some("a"), Some("1"))],
        );
        let second = TranscriptMetrics::measure(TurnCount::new(1), None, [call("Read", Some("b"), Some("1234"))]);

        let tallies = tally_tools([&first, &second]);

        assert_eq!(
            tallies,
            [ToolTally {
                tool: "Read".to_string(),
                runs: RunCount::new(2),
                calls: ToolCallCount::new(3),
                repeated_calls: ToolCallCount::new(1),
                largest_result_bytes: Some(ResultBytes::new(4)),
            }]
        );
    }

    #[test]
    fn round_trips_through_json_without_empty_fields() {
        let metrics = TranscriptMetrics::measure(TurnCount::new(2), None, []);

        let json = serde_json::to_value(&metrics).unwrap();

        assert_eq!(
            json,
            serde_json::json!({"turns": 2, "tool_calls": 0, "repeated_tool_calls": 0})
        );
        assert_eq!(serde_json::from_value::<TranscriptMetrics>(json).unwrap(), metrics);
    }
}
