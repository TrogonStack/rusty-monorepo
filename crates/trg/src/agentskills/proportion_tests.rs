use super::*;

#[test]
fn a_rate_nobody_drew_is_not_a_proportion() {
    assert!(Proportion::parse(0, 0).is_err());
    assert!(Proportion::observed(0, 0).is_none());
}

#[test]
fn more_successes_than_draws_is_refused_rather_than_reported() {
    assert!(Proportion::parse(4, 3).is_err());
}

#[test]
fn three_passes_out_of_three_does_not_claim_certainty() {
    let interval = Proportion::parse(3, 3).expect("three of three").interval();

    assert!(interval.low() < 0.5, "a floor of {} reads as proof", interval.low());
    assert!((interval.high() - 1.0).abs() < 1e-12);
}

#[test]
fn an_interval_never_runs_past_the_ends_of_the_scale() {
    for trials in 1..=20 {
        for successes in 0..=trials {
            let interval = Proportion::parse(successes, trials)
                .expect("a drawn proportion")
                .interval();
            assert!(
                interval.low() >= 0.0 && interval.high() <= 1.0,
                "{successes}/{trials} gave {interval:?}"
            );
        }
    }
}

#[test]
fn a_known_interval_matches_the_published_value() {
    let interval = Proportion::parse(81, 263).expect("a drawn proportion").interval();

    assert!((interval.low() - 0.2553).abs() < 5e-4, "low was {}", interval.low());
    assert!((interval.high() - 0.3662).abs() < 5e-4, "high was {}", interval.high());
}

#[test]
fn more_draws_narrow_the_same_rate() {
    let few = Proportion::parse(2, 3).expect("a drawn proportion").interval();
    let many = Proportion::parse(200, 300).expect("a drawn proportion").interval();

    assert!(many.width() < few.width());
}

#[test]
fn a_two_thirds_lead_over_three_draws_has_not_separated_the_arms() {
    let left = Proportion::parse(2, 3).expect("a drawn proportion");
    let right = Proportion::parse(0, 3).expect("a drawn proportion");

    let interval = difference(left, right);

    assert!(
        (left.rate() - right.rate() - 0.6667).abs() < 5e-4,
        "the subtraction moved"
    );
    assert!(
        !interval.separates_the_arms(),
        "a lead of two thirds over three draws read as a finding at {interval:?}"
    );
}

#[test]
fn a_whole_sweep_of_enough_draws_does_separate_the_arms() {
    let left = Proportion::parse(40, 40).expect("a drawn proportion");
    let right = Proportion::parse(0, 40).expect("a drawn proportion");

    assert!(difference(left, right).separates_the_arms());
}

#[test]
fn two_arms_that_drew_the_same_rate_straddle_no_difference() {
    let arm = Proportion::parse(5, 10).expect("a drawn proportion");

    let interval = difference(arm, arm);

    assert!(interval.low() < 0.0 && interval.high() > 0.0);
    assert!(!interval.separates_the_arms());
}

#[test]
fn an_interval_that_runs_backwards_is_refused() {
    assert!(Interval::parse(0.4, 0.1).is_err());
    assert!(Interval::parse(f64::NAN, 0.1).is_err());
}

#[test]
fn a_case_with_no_graded_attempt_has_no_k_outcome() {
    assert_eq!(case_k_outcome(std::iter::empty()), None);
}

#[test]
fn one_pass_among_several_attempts_is_a_pass_at_k_but_not_a_pass_pow_k() {
    assert_eq!(case_k_outcome([false, true, false]), Some((true, false)));
}

#[test]
fn every_attempt_passing_is_both_a_pass_at_k_and_a_pass_pow_k() {
    assert_eq!(case_k_outcome([true, true]), Some((true, true)));
}

#[test]
fn every_attempt_failing_is_neither() {
    assert_eq!(case_k_outcome([false, false]), Some((false, false)));
}

#[test]
fn k_case_counts_pools_outcomes_across_cases() {
    let counts = k_case_counts([(true, false), (true, true), (false, false)]);

    assert_eq!(counts.trials, 3);
    assert_eq!(counts.any_pass, 2);
    assert_eq!(counts.all_pass, 1);
}

#[test]
fn best_of_n_reads_higher_than_the_matched_aggregate_over_the_same_cases() {
    let cases = [(true, false), (true, false), (true, true)];
    let counts = k_case_counts(cases);

    let pass_at_k = ScoredRate::from_counts(counts.any_pass, counts.trials);
    let pass_pow_k = ScoredRate::from_counts(counts.all_pass, counts.trials);

    assert_eq!(pass_at_k.rate, Some(1.0));
    assert_eq!(pass_pow_k.rate, Some(1.0 / 3.0));
    assert!(pass_pow_k.rate.unwrap() < pass_at_k.rate.unwrap());
}

#[test]
fn a_scored_rate_over_nothing_reports_no_rate_rather_than_dividing_by_zero() {
    let rate = ScoredRate::from_counts(0, 0);

    assert_eq!(rate.rate, None);
    assert_eq!(rate.interval, None);
    assert_eq!(rate.trials, 0);
}

#[test]
fn a_scored_rate_deserializes_through_proportion_parse_rather_than_trusting_its_own_rate() {
    let decoded: ScoredRate =
        serde_json::from_str(r#"{"successes":2,"trials":4,"rate":0.9,"interval":null}"#).expect("valid counts");

    assert_eq!(decoded, ScoredRate::from_counts(2, 4));
    assert_eq!(decoded.rate, Some(0.5));
}

#[test]
fn a_scored_rate_with_more_successes_than_trials_is_refused_on_decode() {
    let result: Result<ScoredRate, _> =
        serde_json::from_str(r#"{"successes":5,"trials":3,"rate":null,"interval":null}"#);

    assert!(result.is_err());
}

#[test]
fn a_scored_rate_over_nothing_decodes_back_to_the_same_default() {
    let decoded: ScoredRate =
        serde_json::from_str(r#"{"successes":0,"trials":0,"rate":null,"interval":null}"#).expect("empty counts");

    assert_eq!(decoded, ScoredRate::default());
}
