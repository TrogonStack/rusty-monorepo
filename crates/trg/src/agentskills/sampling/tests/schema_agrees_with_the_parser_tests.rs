use super::super::*;

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&serde_json::to_value(schemars::schema_for!(AttemptCount)).unwrap()).unwrap()
}

#[test]
fn the_schema_refuses_every_value_deserialize_refuses() {
    let validator = validator();
    for refused in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(4294967296u64),
        serde_json::json!("3"),
        serde_json::json!(1.5),
    ] {
        assert!(
            serde_json::from_value::<AttemptCount>(refused.clone()).is_err(),
            "deserialize admitted {refused}"
        );
        assert!(!validator.is_valid(&refused), "the schema admitted {refused}");
    }
}

#[test]
fn the_schema_admits_every_count_deserialize_admits() {
    let validator = validator();
    for admitted in [
        serde_json::json!(1),
        serde_json::json!(3),
        serde_json::json!(4294967295u32),
    ] {
        assert!(
            serde_json::from_value::<AttemptCount>(admitted.clone()).is_ok(),
            "deserialize refused {admitted}"
        );
        assert!(validator.is_valid(&admitted), "the schema refused {admitted}");
    }
}
