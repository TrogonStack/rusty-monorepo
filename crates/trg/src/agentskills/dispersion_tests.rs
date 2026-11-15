use super::*;

fn sample(scenario: ScenarioKind, attempt: u32, value: u64) -> MetricSample<u32> {
    MetricSample::new(scenario, attempt, value)
}

#[test]
fn a_group_smaller_than_the_floor_reports_nothing() {
    let floor = SampleFloor::dispersion();
    assert_eq!(floor.count(), 4);
    assert!(!floor.admits(3));
    assert!(floor.admits(4));

    let samples = vec![
        sample(ScenarioKind::WithSkill, 1, 1_000),
        sample(ScenarioKind::WithSkill, 2, 1_100),
        sample(ScenarioKind::WithSkill, 3, 9_000),
    ];
    assert!(outliers(samples).is_empty());
}

#[test]
fn a_floor_that_accepts_a_group_with_no_spread_is_refused() {
    assert!(SampleFloor::parse(0).is_err());
    assert!(SampleFloor::parse(1).is_err());
    assert_eq!(SampleFloor::parse(2).unwrap().count(), 2);
}

/// The reason the verdict is median and MAD rather than mean and sigma: the extreme
/// draw drags the mean up and inflates sigma enough that it falls inside its own
/// band, so the statistic that is supposed to catch it reports it as ordinary.
#[test]
fn a_single_extreme_draw_does_not_hide_behind_the_band_it_widened() {
    let values = [98_u64, 100, 102, 10_000];
    let mean = values.iter().map(|value| *value as f64).sum::<f64>() / values.len() as f64;
    let sigma = (values
        .iter()
        .map(|value| {
            let delta = *value as f64 - mean;
            delta * delta
        })
        .sum::<f64>()
        / values.len() as f64)
        .sqrt();
    assert!(10_000.0 < mean + 2.0 * sigma);

    let dispersion = Dispersion::measure(&values).unwrap();
    assert_eq!(dispersion.centre(), 101.0);
    assert_eq!(dispersion.spread(), 2.0);
    assert!(!dispersion.admits(10_000));
    assert!(dispersion.admits(102));
}

#[test]
fn a_group_where_most_draws_are_identical_reports_nothing() {
    assert!(Dispersion::measure(&[1_000, 1_000, 1_000, 1_000, 1_000, 1_000, 1_000, 50_000]).is_none());
    assert!(Dispersion::parse(1_000.0, 0.0).is_err());
    assert!(Dispersion::measure(&[]).is_none());
}

/// The arms of a comparison pay different costs by construction, so pooling them
/// publishes the slower arm as a field of outliers against the faster arm's centre.
#[test]
fn the_arms_of_a_comparison_are_judged_against_themselves() {
    let samples = vec![
        sample(ScenarioKind::WithSkill, 1, 100),
        sample(ScenarioKind::WithSkill, 2, 104),
        sample(ScenarioKind::WithSkill, 3, 96),
        sample(ScenarioKind::WithSkill, 4, 102),
        sample(ScenarioKind::WithoutSkill, 1, 900),
        sample(ScenarioKind::WithoutSkill, 2, 940),
        sample(ScenarioKind::WithoutSkill, 3, 880),
        sample(ScenarioKind::WithoutSkill, 4, 920),
    ];

    assert!(outliers(samples).is_empty());
}

#[test]
fn a_draw_its_own_arm_cannot_account_for_is_reported_with_the_band_it_missed() {
    let samples = vec![
        sample(ScenarioKind::WithSkill, 1, 100),
        sample(ScenarioKind::WithSkill, 2, 104),
        sample(ScenarioKind::WithSkill, 3, 96),
        sample(ScenarioKind::WithSkill, 4, 102),
        sample(ScenarioKind::WithSkill, 5, 5_000),
    ];

    let found = outliers(samples);
    assert_eq!(found.len(), 1);
    let (sample, dispersion) = found.into_iter().next().unwrap().into_parts();
    assert_eq!(sample.value(), 5_000);
    assert_eq!(*sample.origin(), 5);
    assert_eq!(dispersion.centre(), 102.0);
    assert_eq!(dispersion.cutoff(), dispersion.centre() + 3.0 * dispersion.spread());
}

/// Two cases are free to word a check the same way. Pooled on the text, a check that
/// always holds for one case and always fails for another reads as a single check
/// that flips, and no attempt of either case ever flipped.
#[test]
fn a_check_two_cases_word_alike_is_not_one_flaky_check() {
    let mut ledger = FlakinessLedger::default();
    for attempt in 1..=3 {
        ledger.observe(
            AssertionKey::parse("case-a", ScenarioKind::WithSkill, "mentions the total").unwrap(),
            attempt,
            true,
        );
        ledger.observe(
            AssertionKey::parse("case-b", ScenarioKind::WithSkill, "mentions the total").unwrap(),
            attempt,
            false,
        );
    }

    assert!(ledger.flaky_assertions().is_empty());
}

#[test]
fn a_check_that_flipped_carries_how_often_it_did() {
    let mut ledger = FlakinessLedger::default();
    let key = AssertionKey::parse("case-a", ScenarioKind::WithSkill, "  mentions the total  ").unwrap();
    assert_eq!(key.assertion(), "mentions the total");
    for (attempt, passed) in [(1, true), (2, false), (3, true), (4, true)] {
        ledger.observe(key.clone(), attempt, passed);
    }

    let records = ledger.flaky_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].eval_case_id, "case-a");
    assert_eq!(records[0].scenario_id, "with_skill");
    assert_eq!(records[0].attempts, 4);
    assert_eq!(records[0].pass_count, 3);
    assert!((records[0].flakiness_ratio.value() - 0.25).abs() < 0.0001);
}

#[test]
fn the_same_arm_of_the_same_case_is_one_key_per_assertion() {
    let mut ledger = FlakinessLedger::default();
    let with_skill = AssertionKey::parse("case-a", ScenarioKind::WithSkill, "holds").unwrap();
    let without_skill = AssertionKey::parse("case-a", ScenarioKind::WithoutSkill, "holds").unwrap();
    ledger.observe(with_skill.clone(), 1, true);
    ledger.observe(with_skill, 2, true);
    ledger.observe(without_skill.clone(), 1, true);
    ledger.observe(without_skill, 2, false);

    let records = ledger.flaky_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].scenario_id, "without_skill");
}

#[test]
fn an_assertion_with_no_text_names_no_check() {
    assert!(AssertionKey::parse("case-a", ScenarioKind::WithSkill, "   ").is_err());
}

#[test]
fn a_tally_counting_outcomes_that_were_never_drawn_is_refused() {
    assert!(AttemptTally::parse(0, 0).is_err());
    assert!(AttemptTally::parse(2, 3).is_err());

    let tally = AttemptTally::parse(12, 11).unwrap();
    assert_eq!(tally.fail_count(), 1);
    assert!(tally.is_split());
    assert!((tally.flakiness_ratio().percent() - 100.0 / 12.0).abs() < 0.0001);
}

#[test]
fn a_check_that_never_passed_is_broken_rather_than_unstable() {
    assert!(!AttemptTally::parse(4, 0).unwrap().is_split());
    assert!(!AttemptTally::parse(4, 4).unwrap().is_split());
}

#[test]
fn a_ratio_outside_the_share_of_attempts_is_refused() {
    assert!(FlakinessRatio::parse(-0.1).is_err());
    assert!(FlakinessRatio::parse(1.1).is_err());
    assert!(FlakinessRatio::parse(f64::NAN).is_err());
    assert_eq!(FlakinessRatio::parse(0.5).unwrap().value(), 0.5);
}

#[test]
fn a_ratio_round_trips_through_the_field_an_artifact_publishes() {
    let ratio = FlakinessRatio::parse(0.25).unwrap();
    let published = serde_json::to_value(ratio).unwrap();
    assert_eq!(published, serde_json::json!(0.25));
    assert_eq!(serde_json::from_value::<FlakinessRatio>(published).unwrap(), ratio);
    assert!(serde_json::from_value::<FlakinessRatio>(serde_json::json!(2.0)).is_err());
}
