use std::fmt;
use std::str::FromStr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::canonical_json::canonical_bytes;
use crate::constants::{PHX_REF_MAX_BYTES, VIEW_REF_BYTES, VIEW_REF_DOMAIN_TAG};
use crate::entropy::{encode_id, random_id_bytes, EntropyError};
use crate::meta::Meta;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PhxRefError {
    #[error("phx_ref is empty")]
    Empty,
    #[error("phx_ref is {len} bytes, the limit is {max}")]
    TooLong { len: usize, max: usize },
}

fn validate(value: String) -> Result<String, PhxRefError> {
    if value.is_empty() {
        return Err(PhxRefError::Empty);
    }
    if value.len() > PHX_REF_MAX_BYTES {
        return Err(PhxRefError::TooLong {
            len: value.len(),
            max: PHX_REF_MAX_BYTES,
        });
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StoredRef(String);

impl StoredRef {
    pub(crate) fn generate() -> Result<Self, EntropyError> {
        random_id_bytes().map(|bytes| Self(encode_id(&bytes)))
    }

    #[cfg(test)]
    pub(crate) fn parse(text: &str) -> Result<Self, PhxRefError> {
        validate(text.to_owned()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StoredRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for StoredRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for StoredRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        validate(text).map(Self).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ViewRef(String);

impl ViewRef {
    pub fn derive(stored: &StoredRef, meta: &Meta) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(VIEW_REF_DOMAIN_TAG.as_bytes());
        hasher.update((stored.0.len() as u64).to_be_bytes());
        hasher.update(stored.0.as_bytes());
        hasher.update(canonical_bytes(&serde_json::Value::Object(meta.as_map().clone())));
        let digest = hasher.finalize();
        Self(URL_SAFE_NO_PAD.encode(&digest[..VIEW_REF_BYTES]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<StoredRef> for ViewRef {
    fn from(stored: StoredRef) -> Self {
        Self(stored.0)
    }
}

impl From<&StoredRef> for ViewRef {
    fn from(stored: &StoredRef) -> Self {
        Self(stored.0.clone())
    }
}

impl TryFrom<String> for ViewRef {
    type Error = PhxRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate(value).map(Self)
    }
}

impl FromStr for ViewRef {
    type Err = PhxRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<ViewRef> for String {
    fn from(view: ViewRef) -> Self {
        view.0
    }
}

impl fmt::Display for ViewRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn generated_ref_is_22_chars() -> Result<(), EntropyError> {
        assert_eq!(StoredRef::generate()?.as_str().len(), 22);
        Ok(())
    }

    #[test]
    fn accepts_foreign_refs_up_to_limit() {
        assert!(StoredRef::parse("F1a2b3c4d5e6").is_ok());
        assert!(StoredRef::parse(&"x".repeat(PHX_REF_MAX_BYTES)).is_ok());
    }

    #[test]
    fn rejects_empty_and_oversized() {
        assert_eq!(StoredRef::parse(""), Err(PhxRefError::Empty));
        assert_eq!(
            "x".repeat(PHX_REF_MAX_BYTES + 1).parse::<ViewRef>(),
            Err(PhxRefError::TooLong {
                len: PHX_REF_MAX_BYTES + 1,
                max: PHX_REF_MAX_BYTES
            })
        );
    }

    #[test]
    fn stored_ref_deserialization_validates() {
        assert!(serde_json::from_str::<StoredRef>(r#""""#).is_err());
        assert!(serde_json::from_str::<StoredRef>(r#""abc""#).is_ok());
    }

    #[test]
    fn view_ref_is_stable_across_member_order_and_bound_to_stored_ref() -> TestResult {
        let stored = StoredRef::parse("abc")?;
        let left: Meta = serde_json::from_str(r#"{"a":1,"b":{"y":2,"x":1}}"#)?;
        let right: Meta = serde_json::from_str(r#"{"b":{"x":1,"y":2},"a":1}"#)?;
        let view = ViewRef::derive(&stored, &left);
        assert_eq!(view, ViewRef::derive(&stored, &right));
        assert_eq!(view.as_str().len(), 22);
        assert_ne!(view, ViewRef::derive(&StoredRef::parse("abd")?, &left));
        assert_eq!(ViewRef::from(stored.clone()).as_str(), stored.as_str());
        Ok(())
    }
}
