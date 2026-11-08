use super::*;

#[test]
fn attempt_outcomes_are_the_values_the_registry_documents() {
    let emitted = [
        AttemptOutcome::Completed,
        AttemptOutcome::TransientFailure,
        AttemptOutcome::RunnerError,
    ]
    .map(AttemptOutcome::as_str);

    assert_eq!(emitted, ["completed", "transient_failure", "runner_error"]);
}

#[test]
fn only_a_transient_failure_is_transient() {
    assert!(AttemptOutcome::TransientFailure.is_transient());
    assert!(!AttemptOutcome::Completed.is_transient());
    assert!(!AttemptOutcome::RunnerError.is_transient());
}
