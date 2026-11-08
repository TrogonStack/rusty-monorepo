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
