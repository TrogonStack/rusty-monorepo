use serde_json::json;

use super::*;

#[test]
fn a_string_number_boolean_and_array_leaf_each_get_their_own_type_constraint() {
    let input = json!({
        "title": "hello",
        "count": 3,
        "urgent": true,
        "labels": ["a", "b"],
        "ignored": null,
    });

    let expect = derive_expect(&input);

    assert_eq!(
        expect.get(&ExpectPath::from("title")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::String
        })
    );
    assert_eq!(
        expect.get(&ExpectPath::from("count")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::Number
        })
    );
    assert_eq!(
        expect.get(&ExpectPath::from("urgent")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::Boolean
        })
    );
    assert_eq!(
        expect.get(&ExpectPath::from("labels")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::Array
        })
    );
    assert_eq!(expect.get(&ExpectPath::from("ignored")), None);
    assert_eq!(expect.len(), 4);
}

#[test]
fn a_nested_object_is_flattened_into_a_dotted_path() {
    let input = json!({"repo": {"owner": "acme"}});

    let expect = derive_expect(&input);

    assert_eq!(
        expect.get(&ExpectPath::from("repo")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::Object
        })
    );
    assert_eq!(
        expect.get(&ExpectPath::from("repo.owner")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::String
        })
    );
    assert_eq!(expect.len(), 2);
}

#[test]
fn an_object_value_gets_its_own_constraint_even_when_empty() {
    let input = json!({"repo": {}});

    let expect = derive_expect(&input);

    assert_eq!(
        expect.get(&ExpectPath::from("repo")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::Object
        })
    );
    assert_eq!(expect.len(), 1);
}

#[test]
fn a_key_that_is_not_a_bare_scalar_is_rendered_double_quoted() {
    let mut expect = BTreeMap::new();
    expect.insert(
        ExpectPath::from("weird key: with colon"),
        ExpectConstraint::TypeName {
            type_name: JsonTypeName::String,
        },
    );
    let answer = RecordedToolAnswer {
        text: "ok".to_string(),
        is_error: false,
    };

    let rendered = render_fixed_mock(&expect, &answer);

    assert!(
        rendered.contains("\"weird key: with colon\": string"),
        "rendered:\n{rendered}"
    );
}

#[test]
fn an_error_answer_is_written_as_the_error_field_with_an_empty_body() {
    let answer = RecordedToolAnswer {
        text: "rate limited".to_string(),
        is_error: true,
    };

    let rendered = render_fixed_mock(&BTreeMap::new(), &answer);

    assert!(rendered.contains("error: \"rate limited\""), "rendered:\n{rendered}");
    assert!(rendered.ends_with("---\n"), "an error mock has no body: {rendered:?}");
}

#[test]
fn a_recorded_body_containing_a_file_placeholder_is_refused_rather_than_written() {
    let temp = tempfile::tempdir().unwrap();
    let mocks_dir = temp.path().join("mocks");
    let server = ServerName::from("probeserver");
    let tool = ToolName::from("probe");
    let answer = RecordedToolAnswer {
        text: "see {{file:secret.json}} for details".to_string(),
        is_error: false,
    };

    let err = write_recorded_mock(&mocks_dir, &server, &tool, &json!({}), &answer).unwrap_err();

    assert!(
        matches!(err, RecordingError::UnrepresentablePlaceholder { .. }),
        "err: {err:?}"
    );
    assert!(!mocks_dir.exists(), "no mock file should have been written: {err}");
}

#[test]
fn a_recorded_error_containing_an_input_placeholder_is_refused_rather_than_written() {
    let temp = tempfile::tempdir().unwrap();
    let mocks_dir = temp.path().join("mocks");
    let server = ServerName::from("probeserver");
    let tool = ToolName::from("probe");
    let answer = RecordedToolAnswer {
        text: "value is {{input.secret}}".to_string(),
        is_error: true,
    };

    let err = write_recorded_mock(&mocks_dir, &server, &tool, &json!({}), &answer).unwrap_err();

    assert!(
        matches!(err, RecordingError::UnrepresentablePlaceholder { .. }),
        "err: {err:?}"
    );
    assert!(!mocks_dir.exists(), "no mock file should have been written: {err}");
}

#[test]
fn a_recorded_error_containing_a_file_placeholder_is_written_as_is() {
    let temp = tempfile::tempdir().unwrap();
    let mocks_dir = temp.path().join("mocks");
    let server = ServerName::from("probeserver");
    let tool = ToolName::from("probe");
    let answer = RecordedToolAnswer {
        text: "see {{file:secret.json}} for details".to_string(),
        is_error: true,
    };

    let path = write_recorded_mock(&mocks_dir, &server, &tool, &json!({}), &answer).unwrap();

    let rendered = fs::read_to_string(&path).unwrap();
    assert!(
        rendered.contains("{{file:secret.json}}"),
        "error field is never expanded on replay, so it should carry the sequence verbatim: {rendered}"
    );
}

#[test]
fn a_result_with_no_text_content_falls_back_to_the_whole_result_as_json() {
    let result = json!({"structuredContent": {"ok": true}});

    let answer = RecordedToolAnswer::from_result(&result);

    assert_eq!(answer.text, serde_json::to_string(&result).unwrap());
    assert!(!answer.is_error);
}

#[test]
fn a_result_with_text_content_uses_the_text_verbatim() {
    let result = json!({"content": [{"type": "text", "text": "issue created"}], "isError": false});

    let answer = RecordedToolAnswer::from_result(&result);

    assert_eq!(answer.text, "issue created");
}

/// A fake MCP server, spoken to over real stdio, so recording is exercised against an
/// actual child process without depending on the network or on any server the machine
/// happens to have installed.
#[cfg(test)]
mod fixture_server_tests;
