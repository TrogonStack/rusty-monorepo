use super::*;

fn validator() -> jsonschema::Validator {
    let schema = serde_json::to_value(schemars::schema_for!(CacheTokens)).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// The schema and `Deserialize` have to refuse the same documents. When the schema is
/// the looser of the two, `verify --mode strict` calls a bundle conformant and the
/// next command to read those same bytes fails on them.
#[test]
fn the_schema_refuses_every_document_deserialize_refuses() {
    let validator = validator();
    for document in [
        serde_json::json!({}),
        serde_json::json!({ "read_tokens": null, "write_tokens": null }),
    ] {
        assert!(
            !validator.is_valid(&document),
            "schema admitted a document Deserialize refuses: {document}"
        );
        assert!(serde_json::from_value::<CacheTokens>(document).is_err());
    }
}

#[test]
fn the_schema_admits_a_value_that_names_one_side() {
    let validator = validator();
    for document in [
        serde_json::json!({ "read_tokens": 10 }),
        serde_json::json!({ "write_tokens": 10 }),
        serde_json::json!({ "read_tokens": 10, "write_tokens": 4 }),
    ] {
        assert!(
            validator.is_valid(&document),
            "schema refused a readable value: {document}"
        );
        assert!(serde_json::from_value::<CacheTokens>(document).is_ok());
    }
}

#[test]
fn naming_neither_side_is_refused() {
    assert!(CacheTokens::parse(None, None).is_err());
}

#[test]
fn naming_either_side_alone_is_accepted() {
    assert!(CacheTokens::parse(Some(5), None).is_ok());
    assert!(CacheTokens::parse(None, Some(5)).is_ok());
}

#[test]
fn deserializing_an_object_that_names_neither_side_is_refused() {
    let result: std::result::Result<CacheTokens, _> = serde_json::from_value(serde_json::json!({}));
    assert!(result.is_err());
}

#[test]
fn a_read_only_value_round_trips_without_gaining_a_write_side() {
    let read_only = CacheTokens::parse(Some(10), None).unwrap();
    let json = serde_json::to_value(read_only).unwrap();
    assert_eq!(json, serde_json::json!({ "read_tokens": 10 }));
    assert_eq!(serde_json::from_value::<CacheTokens>(json).unwrap(), read_only);
}
