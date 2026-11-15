use super::super::*;

fn validator() -> jsonschema::Validator {
    let schema = serde_json::to_value(schemars::schema_for!(JudgeVotes)).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// The schema and `Deserialize` have to refuse the same documents. When the schema is
/// the looser of the two, `eval verify --mode strict` calls a report conformant and
/// the next command to read those same bytes fails on them.
#[test]
fn the_schema_refuses_every_value_deserialize_refuses() {
    let validator = validator();
    for value in [
        serde_json::json!(0),
        serde_json::json!(2),
        serde_json::json!(4),
        serde_json::json!(4_294_967_296u64),
        serde_json::json!(4_294_967_297u64),
    ] {
        assert!(
            serde_json::from_value::<JudgeVotes>(value.clone()).is_err(),
            "expected Deserialize to refuse {value}"
        );
        assert!(!validator.is_valid(&value), "schema admitted {value}");
    }
}

#[test]
fn the_schema_admits_every_panel_size_deserialize_admits() {
    let validator = validator();
    for value in [
        serde_json::json!(1),
        serde_json::json!(3),
        serde_json::json!(4_294_967_295u32),
    ] {
        assert!(serde_json::from_value::<JudgeVotes>(value.clone()).is_ok());
        assert!(validator.is_valid(&value), "schema refused {value}");
    }
}
