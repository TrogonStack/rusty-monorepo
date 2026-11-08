use super::super::*;

fn validator() -> jsonschema::Validator {
    let schema = serde_json::to_value(schemars::schema_for!(PermissionOutcome)).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// The schema and `Deserialize` have to refuse the same documents. When the schema is
/// the looser of the two, `eval verify --mode strict` calls a report conformant and
/// the next command to read those same bytes fails on them.
#[test]
fn the_schema_refuses_every_value_deserialize_refuses() {
    let validator = validator();
    for value in [
        serde_json::json!("acceptEdits"),
        serde_json::json!({}),
        serde_json::json!({ "requested": "unrestricted", "effective": "workspace_write" }),
        serde_json::json!({ "requested": "workspace_write" }),
        serde_json::json!({ "effective": "workspace_write" }),
        serde_json::json!({ "requested": "workspace_write", "effective": "workspace_write", "extra": true }),
    ] {
        assert!(
            serde_json::from_value::<PermissionOutcome>(value.clone()).is_err(),
            "expected Deserialize to refuse {value}"
        );
        assert!(!validator.is_valid(&value), "schema admitted {value}");
    }
}

#[test]
fn the_schema_admits_every_outcome_deserialize_admits() {
    let validator = validator();
    for value in [
        serde_json::json!("workspace_write"),
        serde_json::json!("unrestricted"),
        serde_json::json!({ "requested": "workspace_write", "effective": "workspace_write" }),
        serde_json::json!({ "requested": "workspace_write", "effective": "unrestricted" }),
        serde_json::json!({ "requested": "unrestricted", "effective": "unrestricted" }),
    ] {
        assert!(serde_json::from_value::<PermissionOutcome>(value.clone()).is_ok());
        assert!(validator.is_valid(&value), "schema refused {value}");
    }
}
