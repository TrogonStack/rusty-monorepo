use super::*;
use crate::agentskills::arm_timing::TimingFileInput;

fn arm(duration_ms: &[f64], total_tokens: &[f64], cost_usd: Option<&[f64]>) -> ArmTiming {
    let mut arm = ArmTiming::default();
    for (run, (duration_ms, total_tokens)) in duration_ms.iter().zip(total_tokens).enumerate() {
        arm.record_run(Some(&TimingFileInput {
            duration_ms: Some(*duration_ms as u64),
            total_tokens: Some(*total_tokens as u64),
            cost_usd: cost_usd.map(|costs| costs[run]),
            ..TimingFileInput::default()
        }));
    }
    arm
}

fn tolerances(cost: f64, duration: f64, tokens: f64) -> EfficiencyTolerances {
    EfficiencyTolerances {
        cost: IncreaseTolerance::parse(cost).unwrap(),
        duration: IncreaseTolerance::parse(duration).unwrap(),
        tokens: IncreaseTolerance::parse(tokens).unwrap(),
    }
}

#[test]
fn a_negative_or_unbounded_tolerance_is_refused() {
    assert!(IncreaseTolerance::parse(-0.1).is_err());
    assert!(IncreaseTolerance::parse(f64::INFINITY).is_err());
    assert!(IncreaseTolerance::parse(f64::NAN).is_err());
    assert!("abc".parse::<IncreaseTolerance>().is_err());
    assert_eq!("0.25".parse::<IncreaseTolerance>().unwrap().value(), 0.25);
    assert_eq!(IncreaseTolerance::default().value(), 0.0);
}

#[test]
fn a_deserialized_tolerance_is_held_to_the_same_rule_as_a_flag() {
    assert!(serde_json::from_str::<IncreaseTolerance>("-1").is_err());
    assert_eq!(serde_json::from_str::<IncreaseTolerance>("0.5").unwrap().value(), 0.5);
}

#[test]
fn runs_that_cost_clearly_more_are_costlier_and_gate() {
    let previous = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.10, 0.11, 0.10, 0.12, 0.10]));
    let current = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.30, 0.31, 0.29, 0.32, 0.30]));

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    assert!(matches!(verdicts.cost, CostVerdict::Costlier(_)), "{verdicts:?}");
    assert!(verdicts.regressed(EfficiencyMetric::Cost));
    assert!(!verdicts.regressed(EfficiencyMetric::Duration));
    assert!(matches!(verdicts.duration, DurationVerdict::Indistinguishable(_)));
}

#[test]
fn runs_that_cost_clearly_less_are_cheaper_and_faster() {
    let previous = arm(&[2000.0, 2100.0, 2200.0, 2300.0], &[900.0; 4], Some(&[0.30; 4]));
    let current = arm(&[1000.0, 1100.0, 1200.0, 1300.0], &[400.0; 4], Some(&[0.10; 4]));

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    assert!(matches!(verdicts.cost, CostVerdict::Cheaper(_)));
    assert!(matches!(verdicts.duration, DurationVerdict::Faster(_)));
    assert!(matches!(verdicts.tokens, TokenVerdict::FewerTokens(_)));
}

#[test]
fn an_increase_inside_the_tolerance_is_not_a_regression() {
    let previous = arm(&[1000.0, 1010.0, 990.0, 1005.0, 995.0], &[500.0; 5], None);
    let current = arm(&[1100.0, 1110.0, 1090.0, 1105.0, 1095.0], &[500.0; 5], None);

    let strict = judge_split(&current, &previous, EfficiencyTolerances::default());
    let lenient = judge_split(&current, &previous, tolerances(0.0, 0.2, 0.0));

    assert!(matches!(strict.duration, DurationVerdict::Slower(_)));
    assert!(strict.regressed(EfficiencyMetric::Duration));
    assert!(
        matches!(lenient.duration, DurationVerdict::WithinTolerance(_)),
        "{lenient:?}"
    );
    assert!(!lenient.regressed(EfficiencyMetric::Duration));
}

#[test]
fn a_real_increase_inside_the_tolerance_is_named_as_one_for_every_metric() {
    let previous = arm(
        &[1000.0, 1010.0, 990.0, 1005.0],
        &[500.0, 505.0, 495.0, 500.0],
        Some(&[0.100; 4]),
    );
    let current = arm(
        &[1100.0, 1110.0, 1090.0, 1105.0],
        &[550.0, 555.0, 545.0, 550.0],
        Some(&[0.105; 4]),
    );

    let verdicts = judge_split(&current, &previous, tolerances(0.1, 0.2, 0.15));

    for (status, regressed) in [
        (
            serde_json::to_value(verdicts.cost).unwrap(),
            verdicts.regressed(EfficiencyMetric::Cost),
        ),
        (
            serde_json::to_value(verdicts.duration).unwrap(),
            verdicts.regressed(EfficiencyMetric::Duration),
        ),
        (
            serde_json::to_value(verdicts.tokens).unwrap(),
            verdicts.regressed(EfficiencyMetric::Tokens),
        ),
    ] {
        assert_eq!(status["status"], "within_tolerance", "{verdicts:?}");
        assert!(status["shift"]["interval"]["low"].as_f64().unwrap() > 0.0);
        assert!(!regressed);
    }
    assert!(describe_split_efficiency(&verdicts).contains("duration within tolerance (+100 ms/run"));
}

#[test]
fn a_zero_tolerance_never_reads_an_increase_as_within_tolerance() {
    let previous = arm(&[1000.0, 1010.0, 990.0, 1005.0], &[500.0; 4], None);
    let current = arm(&[1001.0, 1011.0, 991.0, 1006.0], &[500.0; 4], None);

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    assert!(
        !matches!(verdicts.duration, DurationVerdict::WithinTolerance(_)),
        "{verdicts:?}"
    );
    assert!(
        !matches!(verdicts.tokens, TokenVerdict::WithinTolerance(_)),
        "{verdicts:?}"
    );
}

#[test]
fn a_zero_tolerance_reads_every_real_increase_as_a_regression() {
    let previous = arm(&[1000.0; 4], &[500.0; 4], Some(&[0.10; 4]));
    for duration_ms in [[1001.0, 1002.0, 1003.0, 1004.0], [1100.0, 1150.0, 1200.0, 1251.0]] {
        let current = arm(
            &duration_ms,
            &[510.0, 520.0, 530.0, 540.0],
            Some(&[0.11, 0.12, 0.13, 0.14]),
        );

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert!(matches!(verdicts.cost, CostVerdict::Costlier(_)), "{verdicts:?}");
        assert!(matches!(verdicts.duration, DurationVerdict::Slower(_)), "{verdicts:?}");
        assert!(matches!(verdicts.tokens, TokenVerdict::MoreTokens(_)), "{verdicts:?}");
    }
}

#[test]
fn an_interval_whose_high_end_meets_the_allowance_is_within_tolerance() {
    let previous = arm(&[1000.0; 4], &[500.0; 4], None);
    let current = arm(&[1100.0, 1150.0, 1200.0, 1250.0], &[500.0; 4], None);

    let verdicts = judge_split(&current, &previous, tolerances(0.0, 0.25, 0.0));

    let DurationVerdict::WithinTolerance(comparison) = verdicts.duration else {
        panic!("{verdicts:?}");
    };
    assert_eq!(comparison.shift.interval().high(), 250.0);
    assert!(!verdicts.regressed(EfficiencyMetric::Duration));
}

#[test]
fn an_interval_whose_high_end_passes_the_allowance_is_only_an_increase() {
    let previous = arm(&[1000.0; 4], &[500.0; 4], None);
    let current = arm(&[1100.0, 1150.0, 1200.0, 1251.0], &[500.0; 4], None);

    let verdicts = judge_split(&current, &previous, tolerances(0.0, 0.25, 0.0));

    let DurationVerdict::Increased(comparison) = verdicts.duration else {
        panic!("{verdicts:?}");
    };
    assert_eq!(comparison.shift.interval().low(), 100.0);
    assert_eq!(comparison.shift.interval().high(), 251.0);
    assert!(!verdicts.regressed(EfficiencyMetric::Duration));
}

#[test]
fn an_increase_the_runs_cannot_place_against_the_allowance_is_named_the_same_for_every_metric() {
    let previous = arm(&[1000.0; 4], &[500.0; 4], Some(&[0.10; 4]));
    let current = arm(
        &[1100.0, 1150.0, 1200.0, 1251.0],
        &[510.0, 520.0, 530.0, 540.0],
        Some(&[0.11, 0.12, 0.13, 0.14]),
    );

    let verdicts = judge_split(&current, &previous, tolerances(0.25, 0.25, 0.05));

    for (metric, status) in [
        (EfficiencyMetric::Cost, serde_json::to_value(verdicts.cost).unwrap()),
        (
            EfficiencyMetric::Duration,
            serde_json::to_value(verdicts.duration).unwrap(),
        ),
        (EfficiencyMetric::Tokens, serde_json::to_value(verdicts.tokens).unwrap()),
    ] {
        assert_eq!(status["status"], "increased", "{verdicts:?}");
        assert!(!verdicts.regressed(metric));
    }
    assert!(describe_split_efficiency(&verdicts).contains("tokens increased (+25 tokens/run"));
}

#[test]
fn overlapping_runs_are_indistinguishable() {
    let previous = arm(&[1000.0, 3000.0, 1500.0, 2500.0, 2000.0], &[500.0; 5], None);
    let current = arm(&[1100.0, 2900.0, 1600.0, 2400.0, 2100.0], &[500.0; 5], None);

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    assert!(matches!(verdicts.duration, DurationVerdict::Indistinguishable(_)));
    assert!(matches!(
        judge_split(&current, &previous, tolerances(0.0, 0.5, 0.0)).duration,
        DurationVerdict::Indistinguishable(_)
    ));
}

#[test]
fn an_arm_with_an_unpriced_run_is_unmeasured_rather_than_compared() {
    let previous = arm(&[1000.0; 5], &[500.0; 5], None);
    let current = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.30; 5]));

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    assert_eq!(
        verdicts.cost,
        CostVerdict::Unmeasured {
            reason: UnmeasuredReason::NotEveryRunPriced
        }
    );
}

#[test]
fn too_few_runs_leave_every_metric_unmeasured() {
    let previous = arm(&[1000.0; 3], &[500.0; 3], Some(&[0.10; 3]));
    let current = arm(&[9000.0; 3], &[5000.0; 3], Some(&[0.90; 3]));

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    let too_few = UnmeasuredReason::TooFewReadings;
    assert_eq!(verdicts.cost, CostVerdict::Unmeasured { reason: too_few });
    assert_eq!(verdicts.duration, DurationVerdict::Unmeasured { reason: too_few });
    assert_eq!(verdicts.tokens, TokenVerdict::Unmeasured { reason: too_few });
}

#[test]
fn a_side_with_no_completed_runs_is_no_runs_not_a_measurement() {
    let previous = ArmTiming::default();
    let current = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.10; 5]));

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

    assert_eq!(verdicts.cost, CostVerdict::NoRuns);
    assert_eq!(verdicts.duration, DurationVerdict::NoRuns);
    assert_eq!(verdicts.tokens, TokenVerdict::NoRuns);
}

#[test]
fn a_verdict_serializes_under_its_metric_name() {
    let previous = arm(&[1000.0; 4], &[500.0; 4], Some(&[0.10; 4]));
    let current = arm(&[2000.0; 4], &[900.0; 4], Some(&[0.20; 4]));

    let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());
    let published = serde_json::to_value(verdicts).unwrap();

    assert_eq!(published["cost"]["status"], "costlier");
    assert_eq!(published["duration"]["status"], "slower");
    assert_eq!(published["tokens"]["status"], "more_tokens");
    assert_eq!(published["duration"]["shift"]["estimate"], 1000.0);
    assert_eq!(published["duration"]["previous_median"], 1000.0);
}

#[test]
fn a_section_names_only_the_splits_that_regressed() {
    let previous = arm(&[1000.0; 4], &[500.0; 4], None);
    let slower = arm(&[2000.0; 4], &[500.0; 4], None);
    let same = arm(&[1000.0; 4], &[500.0; 4], None);
    let tolerances = EfficiencyTolerances::default();
    let section = EfficiencySection {
        tolerances,
        by_split: BTreeMap::from([
            (EvalSplit::Train, judge_split(&same, &previous, tolerances)),
            (EvalSplit::Test, judge_split(&slower, &previous, tolerances)),
        ]),
    };

    assert_eq!(
        section.regressed_splits(EfficiencyMetric::Duration),
        vec![EvalSplit::Test]
    );
    assert!(section.regressed_splits(EfficiencyMetric::Tokens).is_empty());
}
