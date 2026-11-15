use super::super::*;

fn schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ModelName)).unwrap()
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema()).unwrap()
}

#[test]
fn the_schema_refuses_every_value_deserialize_refuses() {
    let validator = validator();
    for refused in [
        serde_json::json!(""),
        serde_json::json!("   "),
        serde_json::json!(" opus "),
        serde_json::json!("opus "),
        serde_json::json!(" opus"),
    ] {
        assert!(
            serde_json::from_value::<ModelName>(refused.clone()).is_err(),
            "deserialize admitted {refused}"
        );
        assert!(!validator.is_valid(&refused), "the schema admitted {refused}");
    }
}

#[test]
fn the_schema_admits_every_name_deserialize_admits() {
    let validator = validator();
    for admitted in [
        serde_json::json!("opus"),
        serde_json::json!("claude-opus-5"),
        serde_json::json!("gpt-5-codex"),
        serde_json::json!("a"),
        serde_json::json!("a model with spaces inside"),
    ] {
        assert!(
            serde_json::from_value::<ModelName>(admitted.clone()).is_ok(),
            "deserialize refused {admitted}"
        );
        assert!(validator.is_valid(&admitted), "the schema refused {admitted}");
    }
}
