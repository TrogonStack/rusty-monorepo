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
