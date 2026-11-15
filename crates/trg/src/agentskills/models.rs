use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillProperties {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(
        rename = "allowed-tools",
        default,
        deserialize_with = "deserialize_allowed_tools",
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
}

fn deserialize_allowed_tools<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct AllowedToolsVisitor;

    impl<'de> Visitor<'de> for AllowedToolsVisitor {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a space-separated string or a list of strings")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(AllowedToolsVisitor)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let tools: Vec<String> = value.split_whitespace().map(str::to_string).collect();
            Ok(if tools.is_empty() { None } else { Some(tools) })
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut tools = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                tools.push(item);
            }
            Ok(if tools.is_empty() { None } else { Some(tools) })
        }
    }

    deserializer.deserialize_any(AllowedToolsVisitor)
}

impl SkillProperties {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
