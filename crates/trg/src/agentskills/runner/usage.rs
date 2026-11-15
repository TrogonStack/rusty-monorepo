//! Reading the usage block a harness writes when a run ends.
//!
//! Three harnesses spell the same four counts three different ways, and any of them can
//! write a value that is not a count at all. Asking for a field with a chain of `and_then`
//! answered `None` to three different questions at once: the harness sent no usage block,
//! the block did not carry this field, or the field carried something no reader can add
//! up. Only the last is a harness that garbled its own accounting, and reporting it as an
//! absent count told every reader downstream that the run was simply never priced, so a
//! token average, a benchmark delta, and a cost ceiling each quietly dropped a run whose
//! numbers the harness had in fact sent.
//!
//! A garbled field is therefore carried out of the parse as a finding rather than folded
//! into the counts, and the run that carries it says so in its own warnings.

mod constants;
use constants::FOUND_VALUE_LIMIT;
use serde_json::Value;

use super::Runner;
use crate::agentskills::report::CacheTokens;

/// How one harness spells the four counts every harness reports.
///
/// Declared per runner next to the parse that uses it so the three spellings stay three
/// pieces of data rather than three copies of the same reading code, which is how the
/// harnesses drifted apart on what an unreadable value means in the first place.
pub struct UsageFieldNames {
    pub input: &'static str,
    pub output: &'static str,
    pub cache_read: &'static str,
    pub cache_write: &'static str,
}

/// A usage field whose value the harness wrote in a shape that is not a token count.
///
/// Kept apart from the counts because it is not one: it is the harness contradicting its
/// own record, and the run it belongs to reports a count short by whatever the value was
/// supposed to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableUsageField {
    harness: Runner,
    field: &'static str,
    found: String,
}

impl UnreadableUsageField {
    /// What the run says about this field, in the same run-level `warnings` channel that
    /// already carries every other anomaly which cannot fail a run on its own.
    ///
    /// Names the value it found, because the operator's next move is to read the harness's
    /// own transcript for the run, and a warning saying only that a field was unreadable
    /// would send them there without telling them what to look for.
    pub fn warning(&self) -> String {
        format!(
            "the {} harness reported usage.{} as {}, which is not a token count, so this run reports no {} and every total over it is short by whatever that value meant",
            self.harness.display_name(),
            self.field,
            self.found,
            self.field
        )
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => format!("the boolean {flag}"),
        Value::Number(number) => format!("the number {number}"),
        Value::String(text) => {
            if text.chars().count() > FOUND_VALUE_LIMIT {
                let head: String = text.chars().take(FOUND_VALUE_LIMIT).collect();
                format!("the string {head:?} (truncated)")
            } else {
                format!("the string {text:?}")
            }
        }
        Value::Array(_) => "an array".to_string(),
        Value::Object(_) => "an object".to_string(),
    }
}

/// Everything a harness said about one run's tokens, including what it said that no reader
/// could add up.
///
/// One value rather than four counts beside a list of complaints, so a caller cannot
/// publish the counts and leave the complaints behind: reading the tokens and learning
/// that a field was garbled are the same lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessTokenUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_tokens: Option<CacheTokens>,
    unreadable: Vec<UnreadableUsageField>,
}

impl HarnessTokenUsage {
    /// The tokens of a run that never reached the point of reporting any: a run the clock
    /// ended, or one the harness failed before its terminal event. Nobody garbled anything,
    /// so there is nothing to warn about either.
    pub fn unreported() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            unreadable: Vec::new(),
        }
    }

    /// Read one harness's usage block under that harness's own field names.
    ///
    /// `block` is `None` when the harness's terminal event carried no usage block at all,
    /// which reports the same counts as a block that named none of these fields: in both
    /// cases the harness said nothing, and saying nothing is not a contradiction.
    ///
    /// A field present but not readable as a non-negative integer is recorded in
    /// `unreadable` and leaves its count absent. An explicit `null` counts as unreadable
    /// rather than absent: a harness that names a field and fills it with nothing knew the
    /// field existed, which is exactly what the operator chasing the missing count needs to
    /// know.
    pub fn read(block: Option<&Value>, harness: Runner, fields: UsageFieldNames) -> Self {
        let mut unreadable = Vec::new();
        let input_tokens = read_count(block, harness, fields.input, &mut unreadable);
        let output_tokens = read_count(block, harness, fields.output, &mut unreadable);
        let cache_read = read_count(block, harness, fields.cache_read, &mut unreadable);
        let cache_write = read_count(block, harness, fields.cache_write, &mut unreadable);
        let cached_tokens = match (cache_read, cache_write) {
            (None, None) => None,
            (read, write) => Some(CacheTokens::parse(read, write).expect("read or write is Some by the match arm")),
        };
        Self {
            input_tokens,
            output_tokens,
            cached_tokens,
            unreadable,
        }
    }

    pub fn input_tokens(&self) -> Option<u64> {
        self.input_tokens
    }

    pub fn output_tokens(&self) -> Option<u64> {
        self.output_tokens
    }

    pub fn cached_tokens(&self) -> Option<CacheTokens> {
        self.cached_tokens
    }

    /// The one rule for `total_tokens`, so three runners parsing three different harnesses
    /// cannot each answer "what did this run cost in tokens" a different way.
    ///
    /// Deliberately excludes any cached count. Harnesses do not agree on whether a cached
    /// token is billed on top of the input count or already counted inside it, so folding a
    /// cached figure in here would smuggle that disagreement back into a number a reader is
    /// told is harness-independent. It answers only "how much fresh input and output did
    /// this run report"; `cached_tokens` carries the rest.
    pub fn total_tokens(&self) -> Option<u64> {
        match (self.input_tokens, self.output_tokens) {
            (None, None) => None,
            (input, output) => Some(input.unwrap_or(0) + output.unwrap_or(0)),
        }
    }

    /// Every field the harness wrote in a shape that is not a token count. Empty for a
    /// harness that reported nothing, which is the whole point of keeping the two apart.
    pub fn unreadable(&self) -> &[UnreadableUsageField] {
        &self.unreadable
    }

    #[cfg(test)]
    pub fn reported(input_tokens: Option<u64>, output_tokens: Option<u64>, cached_tokens: Option<CacheTokens>) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_tokens,
            unreadable: Vec::new(),
        }
    }
}

fn read_count(
    block: Option<&Value>,
    harness: Runner,
    field: &'static str,
    unreadable: &mut Vec<UnreadableUsageField>,
) -> Option<u64> {
    let value = block.and_then(|block| block.get(field))?;
    match value.as_u64() {
        Some(count) => Some(count),
        None => {
            unreadable.push(UnreadableUsageField {
                harness,
                field,
                found: describe(value),
            });
            None
        }
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
