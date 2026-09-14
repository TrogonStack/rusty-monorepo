//! Text a case asks to be appended to the harness's own system prompt.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a case adds to the instructions its harness starts from.
///
/// Held apart from a bare `String` because a blank appendix is not a weaker instruction,
/// it is a different run: the flag is still passed, the harness still reports having been
/// given one, and a report would name a case as having steered its agent when nothing was
/// added. A case that wants no appendix declares none.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SystemPromptAppendix(String);

impl SystemPromptAppendix {
    pub fn parse(text: impl Into<String>) -> Result<Self, String> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(
                "an appended system prompt cannot be blank: a case that adds nothing to the harness's instructions should declare nothing".to_string(),
            );
        }
        if text.contains('\0') {
            return Err(
                "an appended system prompt cannot hold a NUL byte, which no process can be handed as an argument"
                    .to_string(),
            );
        }
        Ok(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SystemPromptAppendix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for SystemPromptAppendix {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for SystemPromptAppendix {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SystemPromptAppendix {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SystemPromptAppendix".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
    /// schema looser than its own parser lets `eval verify --mode strict` call a suite
    /// conformant that `eval run` then refuses to read.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Text appended to the harness's own system prompt for this case. Not blank.",
            "type": "string",
            "minLength": 1,
            "pattern": "\\S"
        })
    }
}

#[cfg(test)]
mod schema_agrees_with_the_parser {
    use super::*;

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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blank appendix still puts the flag on the command line, so the run is recorded as
    /// having been steered while nothing was added to steer it.
    #[test]
    fn a_blank_appendix_is_refused_rather_than_passed_through() {
        assert!(SystemPromptAppendix::parse("   ").is_err());
    }

    #[test]
    fn an_appendix_reaches_the_harness_exactly_as_written() {
        let appendix = SystemPromptAppendix::parse("  Answer in British English.  ").unwrap();
        assert_eq!(appendix.as_str(), "  Answer in British English.  ");
    }
}
