use super::*;

const FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input_tokens",
    output: "output_tokens",
    cache_read: "cache_read_input_tokens",
    cache_write: "cache_creation_input_tokens",
};

#[test]
fn a_harness_that_sent_no_usage_block_reports_no_counts_and_no_complaint() {
    let usage = HarnessTokenUsage::read(None, Runner::ClaudeCode, FIELDS);

    assert_eq!(usage.total_tokens(), None);
    assert!(usage.unreadable().is_empty());
}

#[test]
fn a_value_that_is_not_a_count_leaves_the_count_absent_rather_than_guessing_at_one() {
    let block = serde_json::json!({ "input_tokens": "eighty", "output_tokens": 20 });

    let usage = HarnessTokenUsage::read(Some(&block), Runner::ClaudeCode, FIELDS);

    assert_eq!(usage.input_tokens(), None);
    assert_eq!(usage.output_tokens(), Some(20));
    assert_eq!(usage.total_tokens(), Some(20));
    assert_eq!(usage.unreadable().len(), 1);
}

#[test]
fn every_shape_a_count_cannot_be_is_named_in_the_warning_it_produces() {
    let block = serde_json::json!({
        "input_tokens": -5,
        "output_tokens": 12.5,
        "cache_read_input_tokens": null,
        "cache_creation_input_tokens": { "total": 7 },
    });

    let usage = HarnessTokenUsage::read(Some(&block), Runner::ClaudeCode, FIELDS);
    let warnings: Vec<String> = usage.unreadable().iter().map(UnreadableUsageField::warning).collect();

    assert_eq!(warnings.len(), 4);
    assert!(
        warnings[0].contains("the number -5"),
        "unexpected warning: {}",
        warnings[0]
    );
    assert!(
        warnings[1].contains("the number 12.5"),
        "unexpected warning: {}",
        warnings[1]
    );
    assert!(warnings[2].contains("as null"), "unexpected warning: {}", warnings[2]);
    assert!(warnings[3].contains("an object"), "unexpected warning: {}", warnings[3]);
    assert!(
        warnings[0].contains("claude-code"),
        "the operator has three harnesses to check, so the warning names which one: {}",
        warnings[0]
    );
}

#[test]
fn a_value_too_long_to_read_is_quoted_only_as_far_as_it_takes_to_recognize_it() {
    let block = serde_json::json!({ "input_tokens": "x".repeat(500) });

    let usage = HarnessTokenUsage::read(Some(&block), Runner::ClaudeCode, FIELDS);
    let warning = usage.unreadable()[0].warning();

    assert!(warning.contains("(truncated)"), "unexpected warning: {warning}");
    assert!(warning.len() < 300, "a warning should not be a transcript: {warning}");
}

/// The shared rule, pinned once so a change to it is a change every runner feels.
#[test]
fn total_tokens_sums_input_and_output_only() {
    assert_eq!(
        HarnessTokenUsage::reported(Some(80), Some(20), None).total_tokens(),
        Some(100)
    );
    assert_eq!(
        HarnessTokenUsage::reported(Some(80), None, None).total_tokens(),
        Some(80)
    );
    assert_eq!(
        HarnessTokenUsage::reported(None, Some(20), None).total_tokens(),
        Some(20)
    );
    assert_eq!(HarnessTokenUsage::reported(None, None, None).total_tokens(), None);
}

#[test]
fn a_cached_count_is_never_folded_into_the_total() {
    let block = serde_json::json!({
        "input_tokens": 80,
        "output_tokens": 20,
        "cache_read_input_tokens": 500,
    });

    let usage = HarnessTokenUsage::read(Some(&block), Runner::ClaudeCode, FIELDS);

    assert_eq!(usage.total_tokens(), Some(100));
    assert_eq!(
        usage.cached_tokens(),
        Some(CacheTokens::parse(Some(500), None).unwrap())
    );
}
