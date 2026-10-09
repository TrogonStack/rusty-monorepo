use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::canonical_json::{depth_of, CanonicalJsonV1};
use crate::constants::{CANONICAL_JSON_MAX_DEPTH, META_MAX_ENCODED_BYTES, META_PROTOTYPE_KEYS, META_RESERVED_KEYS};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(into = "Map<String, Value>")]
pub struct Meta(Map<String, Value>);

#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("meta key {0:?} is reserved")]
    ReservedKey(String),
    #[error("meta encodes to {size} bytes, the limit is {max}")]
    TooLarge { size: usize, max: usize },
    #[error("meta nests deeper than {CANONICAL_JSON_MAX_DEPTH} containers")]
    TooDeep,
    #[error("meta must be a json object")]
    NotAnObject,
    #[error("meta could not be encoded: {0}")]
    Encoding(#[from] serde_json::Error),
}

fn find_prototype_key(value: &Value) -> Option<&str> {
    match value {
        Value::Object(members) => members.iter().find_map(|(key, member)| {
            META_PROTOTYPE_KEYS
                .iter()
                .find(|reserved| **reserved == key)
                .copied()
                .or_else(|| find_prototype_key(member))
        }),
        Value::Array(items) => items.iter().find_map(find_prototype_key),
        _ => None,
    }
}

impl Meta {
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }
}

impl TryFrom<Map<String, Value>> for Meta {
    type Error = MetaError;

    fn try_from(map: Map<String, Value>) -> Result<Self, Self::Error> {
        if let Some(reserved) = META_RESERVED_KEYS.iter().find(|k| map.contains_key(**k)) {
            return Err(MetaError::ReservedKey((*reserved).to_owned()));
        }
        let value = Value::Object(map);
        if let Some(reserved) = find_prototype_key(&value) {
            return Err(MetaError::ReservedKey(reserved.to_owned()));
        }
        if depth_of(&value) > CANONICAL_JSON_MAX_DEPTH {
            return Err(MetaError::TooDeep);
        }
        let Value::Object(map) = value else {
            return Err(MetaError::NotAnObject);
        };
        let size = serde_json::to_vec(&map)?.len();
        if size > META_MAX_ENCODED_BYTES {
            return Err(MetaError::TooLarge {
                size,
                max: META_MAX_ENCODED_BYTES,
            });
        }
        Ok(Self(map))
    }
}

impl<'de> Deserialize<'de> for Meta {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match CanonicalJsonV1::deserialize(deserializer)?.into_value() {
            Value::Object(map) => Meta::try_from(map).map_err(serde::de::Error::custom),
            _ => Err(serde::de::Error::custom(MetaError::NotAnObject)),
        }
    }
}

impl From<Meta> for Map<String, Value> {
    fn from(meta: Meta) -> Self {
        meta.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, Value)]) -> Map<String, Value> {
        entries.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }

    #[test]
    fn accepts_plain_meta() {
        assert!(Meta::try_from(map(&[("status", Value::from("online"))])).is_ok());
    }

    #[test]
    fn rejects_reserved_keys() {
        for key in META_RESERVED_KEYS {
            assert!(matches!(
                Meta::try_from(map(&[(key, Value::Null)])),
                Err(MetaError::ReservedKey(found)) if found == key
            ));
        }
    }

    #[test]
    fn enforces_encoded_cap() {
        let overhead = r#"{"s":""}"#.len();
        let fits = map(&[("s", Value::from("x".repeat(META_MAX_ENCODED_BYTES - overhead)))]);
        assert!(Meta::try_from(fits).is_ok());
        let over = map(&[("s", Value::from("x".repeat(META_MAX_ENCODED_BYTES - overhead + 1)))]);
        assert!(matches!(
            Meta::try_from(over),
            Err(MetaError::TooLarge { size, .. }) if size == META_MAX_ENCODED_BYTES + 1
        ));
    }

    #[test]
    fn rejects_prototype_keys_nested_three_levels_deep() {
        assert!(matches!(
            serde_json::from_str::<Meta>(r#"{"a":{"b":{"c":{"__proto__":{}}}}}"#),
            Err(err) if err.to_string().contains("__proto__")
        ));
        assert!(matches!(
            Meta::try_from(map(&[("a", serde_json::json!([{"b": [{"constructor": 1}]}]))])),
            Err(MetaError::ReservedKey(found)) if found == "constructor"
        ));
        assert!(Meta::try_from(map(&[("a", serde_json::json!({"phx_ref": 1}))])).is_ok());
    }

    #[test]
    fn rejects_duplicate_members_and_excess_depth() {
        assert!(serde_json::from_str::<Meta>(r#"{"a":1,"a":1}"#).is_err());
        let deep = format!("{{\"a\":{}{}}}", "[".repeat(32), "]".repeat(32));
        assert!(serde_json::from_str::<Meta>(&deep).is_err());
        let fits = format!("{{\"a\":{}{}}}", "[".repeat(31), "]".repeat(31));
        assert!(serde_json::from_str::<Meta>(&fits).is_ok());
    }

    #[test]
    fn deserialization_validates() {
        assert!(serde_json::from_str::<Meta>(r#"{"__proto__":{}}"#).is_err());
        assert!(serde_json::from_str::<Meta>(r#"{"status":"away"}"#).is_ok());
    }
}
