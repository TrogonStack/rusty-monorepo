use super::super::*;

#[test]
fn a_bare_string_parses_as_writable() {
    let fixture: EvalFixture = serde_json::from_value(serde_json::json!("evals/files/input.csv")).unwrap();
    assert!(!fixture.is_read_only());
    assert_eq!(fixture.as_str(), "evals/files/input.csv");
}

#[test]
fn a_writable_fixture_round_trips_as_a_bare_string() {
    let fixture = EvalFixture::writable(RelativeSkillPath("evals/files/input.csv".to_string()));
    let value = serde_json::to_value(&fixture).unwrap();
    assert_eq!(value, serde_json::json!("evals/files/input.csv"));
}

#[test]
fn an_object_form_fixture_without_mode_is_writable() {
    let fixture: EvalFixture = serde_json::from_value(serde_json::json!({ "path": "evals/files/input.csv" })).unwrap();
    assert!(!fixture.is_read_only());
}

#[test]
fn an_object_form_fixture_can_declare_read_only() {
    let fixture: EvalFixture = serde_json::from_value(serde_json::json!({
        "path": "evals/files/input.csv",
        "mode": "read_only",
    }))
    .unwrap();
    assert!(fixture.is_read_only());
    assert_eq!(fixture.as_str(), "evals/files/input.csv");
}

#[test]
fn a_read_only_fixture_round_trips_as_an_object() {
    let fixture = EvalFixture::read_only(RelativeSkillPath("evals/files/input.csv".to_string()));
    let value = serde_json::to_value(&fixture).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "path": "evals/files/input.csv", "mode": "read_only" })
    );
}

/// The object form must still refuse an escaping path: `RelativeSkillPath`'s own
/// validation is what a fixture path routes through no matter which wire shape it
/// arrived in, and this is the shape most likely to be built by hand and skip it.
#[test]
fn the_object_form_still_rejects_a_path_that_escapes_the_skill_directory() {
    let error = serde_json::from_value::<EvalFixture>(serde_json::json!({
        "path": "../outside.txt",
        "mode": "read_only",
    }))
    .unwrap_err();
    assert!(error.to_string().contains("must stay inside the skill directory"));
}

#[test]
fn a_bare_string_still_rejects_an_absolute_path() {
    let error = serde_json::from_value::<EvalFixture>(serde_json::json!("/etc/passwd")).unwrap_err();
    assert!(error.to_string().contains("must be relative"));
}

#[test]
fn the_object_form_rejects_unknown_fields() {
    let error = serde_json::from_value::<EvalFixture>(serde_json::json!({
        "path": "evals/files/input.csv",
        "mode": "read_only",
        "unexpected": true,
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unexpected") || error.to_string().contains("unknown field"));
}
