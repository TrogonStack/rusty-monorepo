use super::*;

#[test]
fn a_call_spec_parses_the_tool_name_and_json_input() {
    let spec = RecordedCallSpec::from_str(r#"create_issue={"repo":"acme/widgets"}"#).unwrap();

    assert_eq!(spec.tool.as_str(), "create_issue");
    assert_eq!(spec.input, serde_json::json!({"repo": "acme/widgets"}));
}

#[test]
fn a_call_spec_without_an_equals_sign_is_rejected_by_name() {
    let err = RecordedCallSpec::from_str("create_issue").unwrap_err();

    assert!(err.contains("TOOL=JSON"), "error should name the expected shape: {err}");
}

#[test]
fn a_call_spec_with_invalid_json_is_rejected() {
    let err = RecordedCallSpec::from_str("create_issue=not json").unwrap_err();

    assert!(err.contains("not valid JSON"), "error: {err}");
}

#[test]
fn a_call_spec_whose_input_is_not_a_json_object_is_rejected() {
    for json in ["[1,2,3]", "1", "\"hi\"", "true", "null"] {
        let err = RecordedCallSpec::from_str(&format!("create_issue={json}"))
            .expect_err(&format!("{json} should have been rejected"));
        assert!(err.contains("JSON object"), "json {json:?}: {err}");
    }
}

fn one_call() -> Vec<RecordedCallSpec> {
    vec![RecordedCallSpec::from_str("probe={}").unwrap()]
}

#[test]
fn an_empty_server_name_is_rejected_by_name() {
    let err = validate_flags("", "./mcp-servers/github", &one_call()).unwrap_err();

    assert!(err.contains("--server"), "error should name the empty flag: {err}");
}

#[test]
fn an_empty_command_is_rejected_by_name() {
    let err = validate_flags("github", "", &one_call()).unwrap_err();

    assert!(err.contains("--command"), "error should name the empty flag: {err}");
}

#[test]
fn a_server_name_that_is_not_a_single_path_component_is_rejected() {
    for server in ["../outside", "/etc/passwd", ".", ".."] {
        let err = validate_flags(server, "./mcp-servers/github", &one_call())
            .expect_err(&format!("{server:?} should have been rejected"));
        assert!(err.contains("--server"), "server {server:?}: {err}");
    }
}

#[test]
fn a_call_tool_name_that_is_not_a_single_path_component_is_rejected() {
    for tool in ["../escape", "/abs", ".", ".."] {
        let calls = vec![RecordedCallSpec::from_str(&format!("{tool}={{}}")).unwrap()];
        let err = validate_flags("github", "./mcp-servers/github", &calls)
            .expect_err(&format!("{tool:?} should have been rejected"));
        assert!(err.contains("--call"), "tool {tool:?}: {err}");
    }
}

#[test]
fn duplicate_call_tool_names_are_rejected_before_any_server_is_spawned() {
    let calls = vec![
        RecordedCallSpec::from_str("probe={}").unwrap(),
        RecordedCallSpec::from_str(r#"probe={"x":1}"#).unwrap(),
    ];

    let err = validate_flags("github", "./mcp-servers/github", &calls).unwrap_err();

    assert!(err.contains("probe"), "error should name the repeated tool: {err}");
}
