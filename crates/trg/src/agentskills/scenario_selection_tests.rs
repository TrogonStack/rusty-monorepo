use super::*;

#[test]
fn a_pass_that_names_no_scenario_runs_both_arms() {
    let selection = ScenarioSelection::resolve(&[], false).unwrap();
    assert_eq!(
        selection.scenarios(),
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill]
    );
}

#[test]
fn a_pass_that_names_one_scenario_runs_only_that_one() {
    let selection = ScenarioSelection::resolve(&[ScenarioKind::WithoutSkill], false).unwrap();
    assert_eq!(selection.scenarios(), &[ScenarioKind::WithoutSkill]);
}

#[test]
fn a_pass_that_names_two_scenarios_runs_exactly_those_and_nothing_else() {
    let requested = [ScenarioKind::WithSkill, ScenarioKind::OldSkill];
    let selection = ScenarioSelection::resolve(&requested, false).unwrap();
    assert_eq!(selection.scenarios(), &requested);
}

#[test]
fn a_pass_that_names_no_scenario_under_reuse_completed_runs_the_with_skill_arm_alone() {
    let selection = ScenarioSelection::resolve(&[], true).unwrap();
    assert_eq!(selection.scenarios(), &[ScenarioKind::WithSkill]);
}

#[test]
fn a_pass_that_names_one_scenario_under_reuse_completed_still_runs_only_that_one() {
    let selection = ScenarioSelection::resolve(&[ScenarioKind::WithoutSkill], true).unwrap();
    assert_eq!(selection.scenarios(), &[ScenarioKind::WithoutSkill]);
}

#[test]
fn a_pass_that_names_two_scenarios_under_reuse_completed_is_refused() {
    let requested = [ScenarioKind::WithSkill, ScenarioKind::WithoutSkill];
    assert!(ScenarioSelection::resolve(&requested, true).is_err());
}

#[test]
fn a_pass_that_names_the_same_scenario_twice_under_reuse_completed_is_not_refused() {
    let requested = [ScenarioKind::WithSkill, ScenarioKind::WithSkill];
    let selection = ScenarioSelection::resolve(&requested, true).unwrap();
    assert_eq!(selection.scenarios(), &requested);
}
