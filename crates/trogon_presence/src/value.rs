use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::constants::{VALUE_MAX_ENCODED_BYTES, VALUE_SCHEMA_V2};
use crate::entropy::EntropyError;
use crate::key::PresenceKey;
use crate::meta::Meta;
use crate::operation::{LastOp, WriteIntent};
use crate::phx_ref::StoredRef;
use crate::position::{LifetimeId, MutationSequence, PositionOverflow};
use crate::revision::EntryRevision;
use crate::topic::Topic;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum SchemaVersion {
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stored value schema version {0} is not supported")]
pub struct UnsupportedVersionError(u8);

impl TryFrom<u8> for SchemaVersion {
    type Error = UnsupportedVersionError;

    fn try_from(version: u8) -> Result<Self, Self::Error> {
        match version {
            VALUE_SCHEMA_V2 => Ok(Self::V2),
            other => Err(UnsupportedVersionError(other)),
        }
    }
}

impl From<SchemaVersion> for u8 {
    fn from(version: SchemaVersion) -> Self {
        match version {
            SchemaVersion::V2 => VALUE_SCHEMA_V2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredValue {
    v: SchemaVersion,
    topic: Topic,
    key: PresenceKey,
    phx_ref: StoredRef,
    phx_ref_prev: Option<StoredRef>,
    meta: Meta,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
    last_op: LastOp,
    client_meta: Meta,
    birth_rev: Option<EntryRevision>,
    #[serde(flatten)]
    unknown: Map<String, Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum ValueError {
    #[error("stored value encodes to {size} bytes, the limit is {max}")]
    TooLarge { size: usize, max: usize },
    #[error("stored value is not valid JSON for this schema: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the operation carries no client metadata")]
    MissingClientMeta,
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error(transparent)]
    Overflow(#[from] PositionOverflow),
}

fn capped(bytes: Vec<u8>) -> Result<Vec<u8>, ValueError> {
    if bytes.len() > VALUE_MAX_ENCODED_BYTES {
        return Err(ValueError::TooLarge {
            size: bytes.len(),
            max: VALUE_MAX_ENCODED_BYTES,
        });
    }
    Ok(bytes)
}

impl StoredValue {
    pub(crate) fn born(
        intent: &WriteIntent,
        meta: Meta,
        lifetime: LifetimeId,
        phx_ref: StoredRef,
    ) -> Result<Self, ValueError> {
        let client_meta = intent.client_meta().cloned().ok_or(ValueError::MissingClientMeta)?;
        Ok(Self {
            v: SchemaVersion::V2,
            topic: intent.topic().clone(),
            key: intent.key().clone(),
            phx_ref,
            phx_ref_prev: None,
            meta,
            lifetime,
            mutation_seq: MutationSequence::FIRST,
            last_op: LastOp::of(intent),
            client_meta,
            birth_rev: None,
            unknown: Map::new(),
        })
    }

    pub(crate) fn with_birth(self, birth: EntryRevision) -> Self {
        Self {
            birth_rev: self.birth_rev.or(Some(birth)),
            ..self
        }
    }

    pub fn successor(&self, intent: &WriteIntent, meta: Meta, revision: EntryRevision) -> Result<Self, ValueError> {
        let client_meta = intent.client_meta().cloned().ok_or(ValueError::MissingClientMeta)?;
        let (phx_ref, phx_ref_prev) = if meta == self.meta {
            (self.phx_ref.clone(), self.phx_ref_prev.clone())
        } else {
            (StoredRef::generate()?, Some(self.phx_ref.clone()))
        };
        Ok(Self {
            v: SchemaVersion::V2,
            topic: self.topic.clone(),
            key: self.key.clone(),
            phx_ref,
            phx_ref_prev,
            meta,
            lifetime: self.lifetime,
            mutation_seq: self.mutation_seq.next()?,
            last_op: LastOp::of(intent),
            client_meta,
            birth_rev: self.birth_rev.or(Some(revision)),
            unknown: self.unknown.clone(),
        })
    }

    pub(crate) fn retire(&self, intent: &WriteIntent) -> Result<Tombstone, ValueError> {
        self.retire_with(LastOp::of(intent))
    }

    pub(crate) fn retire_with(&self, last_op: LastOp) -> Result<Tombstone, ValueError> {
        Ok(Tombstone {
            v: SchemaVersion::V2,
            lifetime: self.lifetime,
            mutation_seq: self.mutation_seq.next()?,
            last_op,
        })
    }

    pub fn version(&self) -> SchemaVersion {
        self.v
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn phx_ref(&self) -> &StoredRef {
        &self.phx_ref
    }

    pub fn phx_ref_prev(&self) -> Option<&StoredRef> {
        self.phx_ref_prev.as_ref()
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }

    pub fn mutation_seq(&self) -> MutationSequence {
        self.mutation_seq
    }

    pub fn last_op(&self) -> &LastOp {
        &self.last_op
    }

    pub fn client_meta(&self) -> &Meta {
        &self.client_meta
    }

    pub fn birth_rev(&self) -> Option<EntryRevision> {
        self.birth_rev
    }

    pub fn unknown_fields(&self) -> &Map<String, Value> {
        &self.unknown
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ValueError> {
        capped(serde_json::to_vec(self)?)
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ValueError> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    v: SchemaVersion,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
    last_op: LastOp,
}

impl Tombstone {
    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }

    pub fn mutation_seq(&self) -> MutationSequence {
        self.mutation_seq
    }

    pub fn last_op(&self) -> &LastOp {
        &self.last_op
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ValueError> {
        capped(serde_json::to_vec(self)?)
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ValueError> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holder::HolderId;
    use crate::operation::{EntryTarget, RequestWindow, UnixMillis, WriteRequest};
    use crate::position::OperationId;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const DESIGN_EXAMPLE: &str = r#"{"v":2,"topic":"room:lobby","key":"ana@x.io","phx_ref":"Fq1","phx_ref_prev":null,"meta":{"status":"online"},"lifetime":"AAAAAAAAAAAAAAAAAAAAAA","mutation_seq":"1","last_op":{"id":"AQEBAQEBAQEBAQEBAQEBAQ","fingerprint":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","outcome":"tracked"},"client_meta":{"status":"online"},"birth_rev":null}"#;

    fn request(operation: u8) -> WriteRequest {
        WriteRequest::new(
            OperationId::from([operation; 16]),
            RequestWindow::new(UnixMillis::from(1), UnixMillis::from(2)),
            HolderId::from([2; 16]),
        )
    }

    fn meta(json: &str) -> Result<Meta, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn round_trips_design_example() -> TestResult {
        let value = StoredValue::from_json_bytes(DESIGN_EXAMPLE.as_bytes())?;
        assert_eq!(value.topic().tokens(), "room.lobby");
        assert_eq!(value.key().token(), "ana=40x=2Eio");
        assert_eq!(value.phx_ref_prev(), None);
        assert_eq!(value.mutation_seq(), MutationSequence::FIRST);
        assert_eq!(value.birth_rev(), None);
        assert_eq!(value.to_json_bytes()?, DESIGN_EXAMPLE.as_bytes());
        Ok(())
    }

    #[test]
    fn unknown_fields_round_trip() -> TestResult {
        let input = DESIGN_EXAMPLE.replacen(
            r#""birth_rev":null"#,
            r#""birth_rev":"7","holder":"q3V9hX0bS2mWf1ZkR8aT1A","future":{"x":[1,2]}"#,
            1,
        );
        let value = StoredValue::from_json_bytes(input.as_bytes())?;
        assert_eq!(value.unknown_fields().len(), 2);
        assert_eq!(value.birth_rev(), Some(EntryRevision::from(7)));
        let reparsed = StoredValue::from_json_bytes(&value.to_json_bytes()?)?;
        assert_eq!(reparsed, value);
        assert_eq!(
            reparsed.unknown_fields().get("future"),
            Some(&serde_json::from_str::<Value>(r#"{"x":[1,2]}"#)?)
        );
        Ok(())
    }

    #[test]
    fn rejects_other_versions_and_numeric_sequences() {
        let v1 = DESIGN_EXAMPLE.replacen(r#""v":2"#, r#""v":1"#, 1);
        assert!(StoredValue::from_json_bytes(v1.as_bytes()).is_err());
        let numeric = DESIGN_EXAMPLE.replacen(r#""mutation_seq":"1""#, r#""mutation_seq":1"#, 1);
        assert!(StoredValue::from_json_bytes(numeric.as_bytes()).is_err());
    }

    #[test]
    fn rejects_invalid_fields() {
        let reserved = DESIGN_EXAMPLE.replacen(r#"{"status":"online"}"#, r#"{"__proto__":1}"#, 1);
        assert!(StoredValue::from_json_bytes(reserved.as_bytes()).is_err());
        let empty_topic = DESIGN_EXAMPLE.replacen("room:lobby", "", 1);
        assert!(StoredValue::from_json_bytes(empty_topic.as_bytes()).is_err());
    }

    #[test]
    fn successor_keeps_the_ref_unless_stored_meta_changes() -> TestResult {
        let online = meta(r#"{"status":"online"}"#)?;
        let born = StoredValue::born(
            &request(1).track("room:lobby".parse()?, "ana".parse()?, online.clone()),
            online.clone(),
            LifetimeId::from([3; 16]),
            StoredRef::generate()?,
        )?;
        let target = EntryTarget::new(born.lifetime(), born.mutation_seq());
        let birth = EntryRevision::from(9);
        let noop = request(2).update("room:lobby".parse()?, "ana".parse()?, target, online.clone());
        let same = born.successor(&noop, online, birth)?;
        assert_eq!(same.phx_ref(), born.phx_ref());
        assert_eq!(same.phx_ref_prev(), None);
        assert_eq!(same.mutation_seq().get(), 2);
        assert_eq!(same.birth_rev(), Some(birth));
        assert_eq!(same.last_op().id(), noop.request().operation_id());
        let away = meta(r#"{"status":"away"}"#)?;
        let change = request(3).update("room:lobby".parse()?, "ana".parse()?, target, away.clone());
        let changed = same.successor(&change, away, EntryRevision::from(12))?;
        assert_ne!(changed.phx_ref(), same.phx_ref());
        assert_eq!(changed.phx_ref_prev(), Some(same.phx_ref()));
        assert_eq!(changed.mutation_seq().get(), 3);
        assert_eq!(changed.birth_rev(), Some(birth));
        Ok(())
    }

    #[test]
    fn enforces_encoded_value_cap_across_client_and_enriched_meta() -> TestResult {
        let half = "x".repeat(VALUE_MAX_ENCODED_BYTES / 2);
        let client = meta(&format!(r#"{{"s":"{half}"}}"#))?;
        let enriched = meta(&format!(r#"{{"s":"{half}","e":1}}"#))?;
        let value = StoredValue::born(
            &request(1).track("room:lobby".parse()?, "ana".parse()?, client),
            enriched,
            LifetimeId::from([3; 16]),
            StoredRef::generate()?,
        )?;
        assert!(matches!(value.to_json_bytes(), Err(ValueError::TooLarge { .. })));
        Ok(())
    }
}
