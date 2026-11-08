use super::*;
use crate::agentskills::transcript_metrics::{ObservedToolCall, TurnCount};

/// A harness that prices nothing and a harness that priced everything but this run
/// are two different facts, and the field that held only a number carried neither.
#[test]
fn a_run_no_harness_priced_names_the_harness_and_publishes_no_number() {
    let metrics = RunMetrics {
        cost: Some(RunCost::Unpriced {
            harness: "codex".to_string(),
        }),
        ..RunMetrics::default()
    };

    let json = serde_json::to_value(&metrics).unwrap();

    assert_eq!(json["cost"]["kind"], "unpriced");
    assert_eq!(json["cost"]["harness"], "codex");
    assert!(json["cost_usd"].is_null());
}

/// The field an earlier release wrote is the field its readers still look at.
#[test]
fn a_priced_run_still_publishes_the_bare_number() {
    let metrics = RunMetrics {
        cost: Some(RunCost::Priced { usd: 0.42 }),
        ..RunMetrics::default()
    };

    let json = serde_json::to_value(&metrics).unwrap();

    assert_eq!(json["cost_usd"], 0.42);
    assert_eq!(json["cost"]["kind"], "priced");
}

/// Only a harness that prices its runs ever wrote a number here, so a report
/// carrying one is a priced run however old the release that wrote it.
#[test]
fn a_report_written_before_a_run_could_say_why_it_has_no_price_reads_as_priced() {
    let metrics: RunMetrics = serde_json::from_value(serde_json::json!({ "cost_usd": 0.42 })).unwrap();

    assert_eq!(metrics.cost, Some(RunCost::Priced { usd: 0.42 }));
}

#[test]
fn transcript_metrics_round_trip_and_stay_absent_for_runs_that_never_measured_them() {
    let measured = RunMetrics {
        transcript: Some(TranscriptMetrics::measure(
            TurnCount::new(2),
            Some(900),
            [ObservedToolCall {
                tool: "Read",
                arguments: Some("{}"),
                result: Some("abc"),
            }],
        )),
        ..RunMetrics::default()
    };

    let json = serde_json::to_value(&measured).unwrap();
    let unmeasured = serde_json::to_value(RunMetrics::default()).unwrap();

    assert_eq!(json["transcript"]["tools"][0]["largest_result_bytes"], 3);
    assert_eq!(
        serde_json::from_value::<RunMetrics>(json).unwrap().transcript,
        measured.transcript
    );
    assert!(unmeasured.get("transcript").is_none());
}

#[test]
fn a_report_that_says_why_a_run_has_no_price_is_read_over_the_bare_number() {
    let metrics: RunMetrics = serde_json::from_value(serde_json::json!({
        "cost": { "kind": "unpriced", "harness": "codex" },
        "cost_usd": 0.42
    }))
    .unwrap();

    assert_eq!(
        metrics.cost,
        Some(RunCost::Unpriced {
            harness: "codex".to_string()
        })
    );
}
