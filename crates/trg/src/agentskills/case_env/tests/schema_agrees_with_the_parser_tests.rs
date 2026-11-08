use super::super::*;

fn schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(CaseEnv)).unwrap()
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema()).unwrap()
}

#[test]
fn the_schema_refuses_every_map_deserialize_refuses() {
    let validator = validator();
    for refused in [
        serde_json::json!({ "PATH": "/usr/bin" }),
        serde_json::json!({ "HOME": "/tmp" }),
        serde_json::json!({ "ANTHROPIC_API_KEY": "sk-x" }),
        serde_json::json!({ "EVAL_": "x" }),
        serde_json::json!({ "eval_lower": "x" }),
        serde_json::json!({ "EVAL_bad": "x" }),
        serde_json::json!({ "EVAL_HAS-DASH": "x" }),
    ] {
        assert!(
            serde_json::from_value::<CaseEnv>(refused.clone()).is_err(),
            "parser admitted {refused}"
        );
        assert!(!validator.is_valid(&refused), "schema admitted {refused}");
    }
}

#[test]
fn the_schema_admits_every_map_deserialize_admits() {
    let validator = validator();
    for admitted in [
        serde_json::json!({}),
        serde_json::json!({ "EVAL_A": "1" }),
        serde_json::json!({ "EVAL_FIXTURE_SEED": "42" }),
        serde_json::json!({ "EVAL_MODE_2": "" }),
    ] {
        assert!(
            serde_json::from_value::<CaseEnv>(admitted.clone()).is_ok(),
            "parser refused {admitted}"
        );
        assert!(validator.is_valid(&admitted), "schema refused {admitted}");
    }
}
