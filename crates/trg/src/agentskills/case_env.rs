//! Variables a case asks to be put into its own run's environment.
//!
//! A run inherits nothing by accident, which is the property `--environment` exists to
//! hold. A case that needs a value of its own therefore has to say so, and has to say it
//! in a namespace that cannot be mistaken for the machinery around it.

mod constants;
pub use constants::CASE_ENV_PREFIX;
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The name of a variable a case sets.
///
/// Confined to `EVAL_*` so a case cannot reach past its own question: `PATH` decides which
/// binary runs, `HOME` decides where the harness finds its config, and a credential
/// variable decides whose account pays. A suite that could name those would be editing the
/// isolation the policy promises rather than adding to its own inputs, and it would do it
/// invisibly, since nothing downstream distinguishes a value a case set from one the
/// allowlist admitted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CaseEnvKey(String);

impl CaseEnvKey {
    pub fn parse(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        let Some(rest) = name.strip_prefix(CASE_ENV_PREFIX) else {
            return Err(format!(
                "a case can only set variables named {CASE_ENV_PREFIX}*, so {name:?} is refused: anything else would change the environment the run was promised rather than add to it"
            ));
        };
        if rest.is_empty() {
            return Err(format!(
                "{CASE_ENV_PREFIX:?} is the prefix, not a name: a variable a case sets has to say what it is"
            ));
        }
        if let Some(bad) = rest
            .chars()
            .find(|c| !c.is_ascii_uppercase() && !c.is_ascii_digit() && *c != '_')
        {
            return Err(format!(
                "a variable name can only hold A-Z, 0-9 and underscore, so {name:?} is refused over {bad:?}: a shell that cannot name it cannot read it back"
            ));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CaseEnvKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for CaseEnvKey {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for CaseEnvKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// The variables one case adds to its own run.
///
/// Ordered rather than hashed so two passes over the same suite hand the harness the same
/// environment and hash to the same cache key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CaseEnv(BTreeMap<CaseEnvKey, String>);

impl CaseEnv {
    pub fn parse<K, V>(entries: impl IntoIterator<Item = (K, V)>) -> Result<Self, String>
    where
        K: Into<String>,
        V: Into<String>,
    {
        let mut vars = BTreeMap::new();
        for (name, value) in entries {
            let key = CaseEnvKey::parse(name)?;
            let value = value.into();
            if value.contains('\0') {
                return Err(format!(
                    "the value of {key} holds a NUL byte, which no process can be handed: the run would fail to start rather than answer the case"
                ));
            }
            vars.insert(key, value);
        }
        Ok(Self(vars))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

impl<'de> Deserialize<'de> for CaseEnv {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = BTreeMap::<String, String>::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for CaseEnv {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CaseEnv".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
    /// schema looser than its own parser lets `eval verify --mode strict` call a suite
    /// conformant that `eval run` then refuses to read.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Variables this case adds to its own run's environment. Names are confined to EVAL_* so a case cannot rewrite the environment the policy promised.",
            "type": "object",
            "propertyNames": { "pattern": "^EVAL_[A-Z0-9_]+$" },
            "additionalProperties": { "type": "string" }
        })
    }
}

#[cfg(test)]
mod tests;
