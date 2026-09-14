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

    /// The shape a document that carries no stamp at all holds.
    ///
    /// Pinned to 1 rather than tracking [`Self::current`]: those documents were written
    /// before stamping existed and their shape does not change when ours does, so bumping
    /// `current` must never re-label them as the newer shape.
    pub const fn unstamped() -> Self {
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
    /// Reached only through `#[serde(default)]` on an absent stamp, so it answers "what
    /// shape is a document that carries none", not "what shape do we write".
    fn default() -> Self {
        Self::unstamped()
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
        assert_eq!(SchemaVersion::default(), SchemaVersion::unstamped());
        assert_eq!(SchemaVersion::unstamped().get(), 1);
    }

    /// Pins the one thing that must survive the next bump of `current`: an absent stamp
    /// keeps meaning shape 1, so documents written before stamping are never silently
    /// re-labelled as a shape they were not written to.
    #[test]
    fn the_shape_of_an_unstamped_document_does_not_follow_the_shape_we_write() {
        assert_eq!(SchemaVersion::unstamped().get(), 1);
    }

    #[test]
    fn a_zero_stamp_is_refused_because_it_cannot_be_told_from_an_absent_field() {
        assert!(SchemaVersion::parse(0).is_err());
    }
}
