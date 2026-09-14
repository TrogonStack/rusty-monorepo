//! The model a run is asked to execute under.
//!
//! Held apart from a bare `String` because an empty or blank name is not a weaker request,
//! it is a different one: passed through to a harness it either errors out or silently
//! selects that harness's default, and a report would then name a model the run did not use.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A harness's own identifier for a model, verbatim.
///
/// Not validated against any list of known models: which names a harness accepts is the
/// harness's business and changes without us, so refusing an unrecognized one here would
/// block a run that would have worked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ModelName(String);

impl ModelName {
    pub fn parse(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("a model name cannot be blank: a run has to say which model it asked for".to_string());
        }
        if name.trim() != name {
            return Err(format!(
                "a model name cannot be padded with whitespace: {name:?} would not match the name a harness reports back"
            ));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for ModelName {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl std::fmt::Display for ModelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ModelName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ModelName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ModelName".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
    /// schema looser than its own parser lets `eval verify --mode strict` call a suite
    /// conformant that `eval run` then refuses to read.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A harness's own identifier for a model. Not padded with whitespace, and not blank.",
            "type": "string",
            "minLength": 1,
            "pattern": "^\\S(.*\\S)?$"
        })
    }
}

#[cfg(test)]
mod schema_agrees_with_the_parser {
    use super::*;

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_name_is_refused_because_it_would_read_as_a_model_that_was_never_asked_for() {
        assert!(ModelName::parse("").is_err());
        assert!(ModelName::parse("\t ").is_err());
    }

    #[test]
    fn an_unrecognized_name_is_admitted_because_the_harness_owns_that_list() {
        assert_eq!(
            ModelName::parse("some-model-we-have-never-heard-of").unwrap().as_str(),
            "some-model-we-have-never-heard-of"
        );
    }
}
