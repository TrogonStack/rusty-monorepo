use super::*;

/// The default is the whole point of the type: a pass has to sample by default,
/// because an operator who never passes the flag is exactly the one reading a single
/// draw as if it were a measurement.
#[test]
fn a_pass_samples_a_cell_more_than_once_by_default() {
    assert_eq!(AttemptCount::default().count(), 3);
    assert!(!AttemptCount::default().is_single());
}

#[test]
fn a_cell_nobody_draws_is_refused() {
    assert!(AttemptCount::parse(0).is_err());
    assert_eq!(AttemptCount::parse(1).unwrap(), AttemptCount::single());
}

#[test]
fn an_attempt_count_is_parsed_from_the_command_line_form() {
    assert_eq!("5".parse::<AttemptCount>().unwrap().count(), 5);
    assert!("0".parse::<AttemptCount>().is_err());
    assert!("some".parse::<AttemptCount>().is_err());
}

#[test]
fn every_draw_of_a_cell_is_numbered_and_counted() {
    let three = AttemptCount::recommended();
    assert_eq!(three.draws().collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(three.runs_for(4), 12);
    assert_eq!(AttemptCount::single().runs_for(4), 4);
}

#[cfg(test)]
mod schema_agrees_with_the_parser_tests;
