use super::super::*;

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&serde_json::to_value(schemars::schema_for!(SystemPromptAppendix)).unwrap()).unwrap()
}

#[test]
fn the_schema_refuses_every_value_deserialize_refuses() {
    let validator = validator();
    for refused in ["", "   ", "\n\t "] {
        let value = serde_json::json!(refused);
        assert!(
            serde_json::from_value::<SystemPromptAppendix>(value.clone()).is_err(),
            "parser admitted {value}"
        );
        assert!(!validator.is_valid(&value), "schema admitted {value}");
    }
}

#[test]
fn the_schema_admits_every_value_deserialize_admits() {
    let validator = validator();
    for admitted in [
        "Answer in British English.",
        "a",
        "  padded but not empty  ",
        "line one\nline two",
    ] {
        let value = serde_json::json!(admitted);
        assert!(
            serde_json::from_value::<SystemPromptAppendix>(value.clone()).is_ok(),
            "parser refused {value}"
        );
        assert!(validator.is_valid(&value), "schema refused {value}");
    }
}
