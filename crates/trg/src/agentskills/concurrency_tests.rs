use super::*;

#[test]
fn the_default_is_one_harness_at_a_time() {
    assert!(RunConcurrency::default().is_serial());
    assert_eq!(RunConcurrency::default().lanes(), 1);
}

#[test]
fn a_lane_count_outside_the_supported_range_is_refused() {
    assert!(RunConcurrency::parse(0).is_err(), "a run needs a lane");
    assert!(RunConcurrency::parse(RunConcurrency::MAX).is_ok());
    assert!(RunConcurrency::parse(RunConcurrency::MAX + 1).is_err());
}

#[test]
fn a_lane_count_is_parsed_from_the_command_line_form() {
    assert_eq!("4".parse::<RunConcurrency>().unwrap().lanes(), 4);
    assert!("many".parse::<RunConcurrency>().is_err());
    assert!("0".parse::<RunConcurrency>().is_err());
}

/// A lane with nothing to take is a thread spawned to find the queue empty.
#[test]
fn no_more_lanes_are_started_than_there_is_work_for() {
    let four = RunConcurrency::parse(4).unwrap();
    assert_eq!(four.lanes_for(2), 2);
    assert_eq!(four.lanes_for(9), 4);
    assert_eq!(four.lanes_for(0), 0);
}
