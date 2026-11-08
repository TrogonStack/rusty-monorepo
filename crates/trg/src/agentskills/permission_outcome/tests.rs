use super::*;

#[test]
fn a_grant_with_nothing_to_compare_it_against_has_not_been_widened() {
    let outcome: PermissionOutcome = PermissionGrant::WorkspaceWrite.into();
    assert_eq!(outcome.requested(), PermissionGrant::WorkspaceWrite);
    assert_eq!(outcome.effective(), PermissionGrant::WorkspaceWrite);
    assert!(!outcome.was_widened());
}

#[test]
fn a_harness_that_enforces_more_than_it_was_asked_reports_as_widened() {
    let outcome = PermissionOutcome::new(PermissionGrant::WorkspaceWrite, PermissionGrant::Unrestricted).unwrap();
    assert!(outcome.was_widened());
}

#[test]
fn a_harness_cannot_be_recorded_as_enforcing_less_than_was_requested() {
    assert!(PermissionOutcome::new(PermissionGrant::Unrestricted, PermissionGrant::WorkspaceWrite).is_err());
}

#[test]
fn a_bare_grant_deserializes_as_not_yet_widened() {
    let outcome: PermissionOutcome = serde_json::from_value(serde_json::json!("workspace_write")).unwrap();
    assert_eq!(outcome, PermissionGrant::WorkspaceWrite.into());
}

#[test]
fn default_matches_the_default_grant_with_nothing_to_compare_it_against() {
    assert_eq!(PermissionOutcome::default(), PermissionGrant::default().into());
}

#[cfg(test)]
mod schema_agrees_with_the_parser_tests;
