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

/// Where a run's resolved model came from, once the case's own choice and the operator's
/// `--runner-model` are combined by `effective_model`.
///
/// Kept alongside the model name itself rather than inferred from it, because two runs can
/// resolve to the same name for different reasons, and a reader asking "did the case pin
/// this or did the operator" cannot recover that from the name alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// The eval case named a model of its own, overriding whatever the operator asked for.
    Case,
    /// The case named no model, so the operator's `--runner-model` decided it.
    RunnerFlag,
    /// Neither the case nor the operator named a model, so the harness chose its own
    /// default. No runner reports that choice back, so which model actually ran stays
    /// unknown.
    RunnerDefault,
}

#[cfg(test)]
mod tests;
