use super::*;

fn shift(left: &[f64], right: &[f64]) -> Shift {
    Shift::between(left, right).expect("both arms clear the floor")
}

#[test]
fn an_arm_below_the_floor_has_no_shift_at_all() {
    let enough = [1.0, 2.0, 3.0, 4.0];
    let three = [1.0, 2.0, 3.0];

    assert!(Shift::between(&three, &enough).is_none());
    assert!(Shift::between(&enough, &three).is_none());
    assert!(Shift::between(&[], &enough).is_none());
}

#[test]
fn a_reading_that_is_not_a_number_is_refused_rather_than_sorted() {
    let left = [1.0, 2.0, f64::NAN, 4.0];
    let right = [1.0, 2.0, 3.0, 4.0];

    assert!(Shift::between(&left, &right).is_none());
}

#[test]
fn the_smallest_admitted_arms_reach_the_outermost_differences() {
    assert_eq!(exact_lower_rank(4, 4), 1);
    assert_eq!(exact_lower_rank(3, 3), 0);
    assert_eq!(lower_rank(3, 3), None);
}

#[test]
fn the_exact_ranks_match_the_published_table() {
    assert_eq!(exact_lower_rank(5, 5), 3);
    assert_eq!(exact_lower_rank(6, 6), 6);
    assert_eq!(exact_lower_rank(8, 8), 14);
    assert_eq!(exact_lower_rank(10, 10), 24);
    assert_eq!(exact_lower_rank(4, 10), 6);
}

#[test]
fn the_approximation_lands_within_a_rank_of_the_exact_count_where_both_apply() {
    for (left, right) in [(40, 40), (60, 80), (100, 100)] {
        let exact = exact_lower_rank(left, right) as i64;
        let approximate = approximate_lower_rank(left, right) as i64;
        assert!(
            (exact - approximate).abs() <= 1,
            "{left}x{right}: exact {exact}, approximate {approximate}"
        );
    }
}

#[test]
fn a_constant_offset_is_recovered_exactly() {
    let right = [100.0, 140.0, 90.0, 300.0, 120.0];
    let left: Vec<f64> = right.iter().map(|value| value + 50.0).collect();

    let measured = shift(&left, &right);

    assert_eq!(measured.estimate(), 50.0);
    assert!(measured.interval().low() <= 50.0 && measured.interval().high() >= 50.0);
}

#[test]
fn one_runaway_run_does_not_drag_the_estimate() {
    let right = [100.0, 110.0, 120.0, 130.0];
    let left = [100.0, 110.0, 120.0, 100_000.0];

    let measured = shift(&left, &right);

    assert!(
        measured.estimate().abs() <= 10.0,
        "estimate was {}",
        measured.estimate()
    );
}

#[test]
fn arms_that_never_overlap_are_told_apart() {
    let right = [100.0, 110.0, 120.0, 130.0, 105.0, 115.0];
    let left = [200.0, 210.0, 220.0, 230.0, 205.0, 215.0];

    let measured = shift(&left, &right);

    assert!(
        measured.interval().low() > 0.0,
        "interval was {:?}",
        measured.interval()
    );
}

#[test]
fn arms_drawn_from_the_same_runs_are_not_told_apart() {
    let runs = [100.0, 250.0, 130.0, 400.0, 90.0];

    let measured = shift(&runs, &runs);

    assert_eq!(measured.estimate(), 0.0);
    assert!(!measured.interval().separates_the_arms());
}

#[test]
fn readings_that_all_tie_give_a_point_interval_at_their_difference() {
    let left = [5.0, 5.0, 5.0, 5.0];
    let right = [3.0, 3.0, 3.0, 3.0];

    let measured = shift(&left, &right);

    assert_eq!(measured.estimate(), 2.0);
    assert_eq!(measured.interval().low(), 2.0);
    assert_eq!(measured.interval().high(), 2.0);
}

#[test]
fn heavy_ties_keep_the_interval_closed_around_the_tied_value() {
    let left = [10.0, 10.0, 10.0, 11.0, 10.0];
    let right = [10.0, 10.0, 9.0, 10.0, 10.0];

    let measured = shift(&left, &right);

    assert_eq!(measured.estimate(), 0.0);
    assert!(measured.interval().low() <= 0.0 && measured.interval().high() >= 0.0);
}

#[test]
fn the_same_runs_in_any_order_give_the_same_answer() {
    let left = [7.0, 3.0, 9.0, 1.0, 4.0];
    let right = [2.0, 8.0, 6.0, 5.0];
    let mut reversed_left = left;
    reversed_left.reverse();

    assert_eq!(shift(&left, &right), shift(&reversed_left, &right));
}

#[test]
fn a_large_arm_takes_the_approximate_path_and_still_separates_a_real_shift() {
    let right: Vec<f64> = (0..120).map(|run| (run % 17) as f64).collect();
    let left: Vec<f64> = right.iter().map(|value| value + 5.0).collect();

    let measured = shift(&left, &right);

    assert!(left.len() * right.len() > Shift::EXACT_PAIR_LIMIT);
    assert!(measured.interval().low() > 0.0);
    assert_eq!(measured.estimate(), 5.0);
}
