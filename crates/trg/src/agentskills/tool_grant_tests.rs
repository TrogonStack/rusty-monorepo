use super::*;

#[test]
fn an_empty_grant_is_refused() {
    assert!(ToolGrant::parse(Vec::<String>::new()).is_err());
}

#[test]
fn a_blank_tool_name_is_refused() {
    assert!(ToolGrant::parse(["Bash", "  "]).is_err());
}

#[test]
fn duplicate_names_collapse_to_one() {
    let grant = ToolGrant::parse(["Bash", "Bash", "Read"]).unwrap();
    assert_eq!(grant.tools().collect::<Vec<_>>(), vec!["Bash", "Read"]);
}

#[test]
fn narrowing_keeps_only_what_both_sides_admit() {
    let operator = ToolGrant::parse(["Bash", "Read", "Write"]).unwrap();
    let case = ToolGrant::parse(["Read", "Write", "Edit"]).unwrap();
    let effective = operator.narrowed_by(&case);
    assert_eq!(effective.tools().collect::<Vec<_>>(), vec!["Read", "Write"]);
}

#[test]
fn a_case_cannot_widen_the_operators_grant() {
    let operator = ToolGrant::parse(["Read"]).unwrap();
    let case = ToolGrant::parse(["Read", "Bash", "Write"]).unwrap();
    let effective = operator.narrowed_by(&case);
    assert_eq!(effective.tools().collect::<Vec<_>>(), vec!["Read"]);
}

#[test]
fn disjoint_grants_narrow_to_nothing() {
    let operator = ToolGrant::parse(["Bash"]).unwrap();
    let case = ToolGrant::parse(["Read"]).unwrap();
    let effective = operator.narrowed_by(&case);
    assert_eq!(effective.tools().count(), 0);
}

#[test]
fn neither_side_declaring_a_grant_is_unrestricted() {
    assert_eq!(effective_tool_grant(None, None), None);
}

#[test]
fn a_lone_case_declaration_applies_whole_when_the_operator_named_no_ceiling() {
    let case = ToolGrant::parse(["Read", "Grep"]).unwrap();
    let effective = effective_tool_grant(None, Some(&case));
    assert_eq!(effective, Some(case));
}

#[test]
fn a_lone_operator_grant_applies_whole_when_the_case_declares_nothing() {
    let operator = ToolGrant::parse(["Read", "Grep"]).unwrap();
    let effective = effective_tool_grant(Some(&operator), None);
    assert_eq!(effective, Some(operator));
}

#[test]
fn both_sides_declaring_a_grant_narrows_to_their_intersection() {
    let operator = ToolGrant::parse(["Read", "Bash"]).unwrap();
    let case = ToolGrant::parse(["Read"]).unwrap();
    let effective = effective_tool_grant(Some(&operator), Some(&case));
    assert_eq!(effective, Some(ToolGrant::parse(["Read"]).unwrap()));
}

#[test]
fn round_trips_through_json_sorted() {
    let grant = ToolGrant::parse(["Write", "Bash", "Read"]).unwrap();
    let json = serde_json::to_string(&grant).unwrap();
    assert_eq!(json, r#"["Bash","Read","Write"]"#);
    let back: ToolGrant = serde_json::from_str(&json).unwrap();
    assert_eq!(back, grant);
}

#[test]
fn displays_as_a_comma_joined_sorted_list() {
    let grant = ToolGrant::parse(["Write", "Bash"]).unwrap();
    assert_eq!(grant.to_string(), "Bash,Write");
}
