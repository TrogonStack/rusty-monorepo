use super::*;

/// The prefix is the whole isolation guarantee: a case that could name `PATH` decides
/// which binary the harness is, and one that could name a credential variable decides
/// whose account pays, neither of which is a question the suite is asking.
#[test]
fn a_case_cannot_set_a_variable_outside_its_own_namespace() {
    let error = CaseEnv::parse([("PATH", "/evil/bin")]).unwrap_err();
    assert!(error.contains("PATH"), "{error}");
    assert!(error.contains(CASE_ENV_PREFIX), "{error}");
}

#[test]
fn a_case_can_set_a_variable_in_its_own_namespace() {
    let env = CaseEnv::parse([("EVAL_SEED", "42")]).unwrap();
    assert_eq!(env.iter().collect::<Vec<_>>(), vec![("EVAL_SEED", "42")]);
}

/// Two passes over one suite have to hand the harness the same environment, or the
/// cache key cut from it would differ between passes that asked the same question.
#[test]
fn the_variables_come_back_in_a_stable_order() {
    let env = CaseEnv::parse([("EVAL_B", "2"), ("EVAL_A", "1"), ("EVAL_C", "3")]).unwrap();
    assert_eq!(
        env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        vec!["EVAL_A", "EVAL_B", "EVAL_C"]
    );
}

#[cfg(test)]
mod schema_agrees_with_the_parser_tests;
