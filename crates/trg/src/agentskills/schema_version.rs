//! The version stamp every emitted artifact carries.
//!
//! The eval input manifest has declared a `schema_version` since it existed, and our own
//! divergences doc tells consumers to gate on it. Nothing trg emitted carried one, so that
//! instruction was unfollowable for every artifact a consumer actually reads: a reader
//! holding a `report.json` could not tell a document written this release from one written
//! two shapes ago, and had to guess from which keys happened to be present.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which shape of an emitted artifact a document was written to.
///
/// Zero is refused so that a stamp is always a claim about a real shape: `0` is what an
/// absent integer field deserializes to by default in most readers, and admitting it would
/// make "written before stamps existed" and "written to shape zero" the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// The shape `trg` writes today.
    ///
    /// Every emitted artifact shares one number rather than versioning apart: they are
    /// written by one binary in one pass and read as a set, so a consumer asking "can I
    /// read this bundle" is asking one question, not nine.
    pub const fn current() -> Self {
        Self(1)
    }

    pub fn parse(version: u32) -> Result<Self, String> {
        if version == 0 {
            return Err(
                "schema_version must be at least 1: zero is indistinguishable from an absent field".to_string(),
            );
        }
        Ok(Self(version))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for SchemaVersion {
    /// An artifact written before any artifact was stamped is shape 1, which is what
    /// those documents in fact hold: the stamp was added without changing their shape.
    fn default() -> Self {
        Self::current()
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = u32::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for SchemaVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SchemaVersion".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
    /// schema looser than its own parser lets `eval verify --mode strict` call a report
    /// conformant that `grade`, `benchmark` and `compare` then refuse to read.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Which shape of this artifact the document was written to. Absent means it predates stamping, which is shape 1.",
            "type": "integer",
            "minimum": 1,
            "maximum": 4294967295u32
        })
    }
}

#[cfg(test)]
mod schema_agrees_with_the_parser {
    use super::*;

    fn validator() -> jsonschema::Validator {
        let schema = serde_json::to_value(schemars::schema_for!(SchemaVersion)).unwrap();
        jsonschema::validator_for(&schema).unwrap()
    }

    #[test]
    fn the_schema_refuses_every_value_deserialize_refuses() {
        let validator = validator();
        for value in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(4_294_967_296u64),
            serde_json::json!("1"),
            serde_json::json!(1.5),
        ] {
            assert!(
                serde_json::from_value::<SchemaVersion>(value.clone()).is_err(),
                "expected Deserialize to refuse {value}"
            );
            assert!(!validator.is_valid(&value), "schema admitted {value}");
        }
    }

    #[test]
    fn the_schema_admits_every_version_deserialize_admits() {
        let validator = validator();
        for value in [
            serde_json::json!(1),
            serde_json::json!(2),
            serde_json::json!(4_294_967_295u32),
        ] {
            assert!(serde_json::from_value::<SchemaVersion>(value.clone()).is_ok());
            assert!(validator.is_valid(&value), "schema refused {value}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_artifact_written_before_stamping_reads_as_the_shape_it_actually_holds() {
        assert_eq!(SchemaVersion::default(), SchemaVersion::current());
        assert_eq!(SchemaVersion::current().get(), 1);
    }

    #[test]
    fn a_zero_stamp_is_refused_because_it_cannot_be_told_from_an_absent_field() {
        assert!(SchemaVersion::parse(0).is_err());
    }
}
