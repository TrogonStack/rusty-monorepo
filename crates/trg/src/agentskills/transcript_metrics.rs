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
#[path = "transcript_metrics_tests.rs"]
mod tests;
