use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::batch::BatchPosition;
use crate::canonical_json::OperationFingerprint;
use crate::constants::{GUARD_SCHEMA_V1, RECEIPT_MAX_ENCODED_BYTES, RECEIPT_SCHEMA_V1};
use crate::holder::HolderId;
use crate::kv_key::KvKey;
use crate::operation::{OperationKind, OperationResult, ReleaseIntent, WriteIntent};
use crate::phx_ref::StoredRef;
use crate::position::{LifetimeId, MutationSequence, OwnerId, PositionOverflow};
use crate::revision::EntryRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchemaTag<const V: u8>;

impl<const V: u8> Serialize for SchemaTag<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(V)
    }
}

impl<'de, const V: u8> Deserialize<'de> for SchemaTag<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            found if found == V => Ok(Self),
            found => Err(D::Error::custom(format!("schema version {found} is not supported"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptTarget {
    entry: KvKey,
    position: BatchPosition,
    lifetime: LifetimeId,
    stored_ref: StoredRef,
    sequence: MutationSequence,
}

impl ReceiptTarget {
    pub(crate) fn new(
        entry: KvKey,
        position: BatchPosition,
        lifetime: LifetimeId,
        stored_ref: StoredRef,
        sequence: MutationSequence,
    ) -> Self {
        Self {
            entry,
            position,
            lifetime,
            stored_ref,
            sequence,
        }
    }

    pub fn entry(&self) -> &KvKey {
        &self.entry
    }

    pub fn position(&self) -> BatchPosition {
        self.position
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }

    pub fn stored_ref(&self) -> &StoredRef {
        &self.stored_ref
    }

    pub fn sequence(&self) -> MutationSequence {
        self.sequence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasedTarget {
    #[serde(rename = "l")]
    lifetime: LifetimeId,
    #[serde(rename = "s")]
    sequence: MutationSequence,
}

impl ReleasedTarget {
    pub(crate) fn new(lifetime: LifetimeId, sequence: MutationSequence) -> Self {
        Self { lifetime, sequence }
    }

    pub fn lifetime(self) -> LifetimeId {
        self.lifetime
    }

    pub fn sequence(self) -> MutationSequence {
        self.sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    v: SchemaTag<RECEIPT_SCHEMA_V1>,
    fingerprint: OperationFingerprint,
    kind: OperationKind,
    result: OperationResult,
    position: BatchPosition,
    targets: Vec<ReceiptTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    released: Vec<Option<ReleasedTarget>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("receipt encodes to {size} bytes, the limit is {max}")]
    TooLarge { size: usize, max: usize },
    #[error("receipt is not valid JSON for this schema: {0}")]
    Json(#[from] serde_json::Error),
    #[error("receipt target sits before its receipt at position {0}")]
    TargetBeforeReceipt(u16),
    #[error("receipt does not name entry {0}")]
    MissingTarget(KvKey),
    #[error(transparent)]
    Overflow(#[from] PositionOverflow),
}

impl Receipt {
    pub(crate) fn new(intent: &WriteIntent, position: BatchPosition, targets: Vec<ReceiptTarget>) -> Self {
        Self {
            v: SchemaTag,
            fingerprint: intent.fingerprint(),
            kind: intent.kind(),
            result: intent.kind().result(),
            position,
            targets,
            released: Vec::new(),
        }
    }

    pub(crate) fn release(
        intent: &ReleaseIntent,
        position: BatchPosition,
        released: Vec<Option<ReleasedTarget>>,
    ) -> Self {
        Self {
            v: SchemaTag,
            fingerprint: intent.fingerprint(),
            kind: OperationKind::Release,
            result: OperationResult::Released,
            position,
            targets: Vec::new(),
            released,
        }
    }

    pub fn released(&self) -> &[Option<ReleasedTarget>] {
        &self.released
    }

    pub fn fingerprint(&self) -> OperationFingerprint {
        self.fingerprint
    }

    pub fn kind(&self) -> OperationKind {
        self.kind
    }

    pub fn result(&self) -> OperationResult {
        self.result
    }

    pub fn position(&self) -> BatchPosition {
        self.position
    }

    pub fn targets(&self) -> &[ReceiptTarget] {
        &self.targets
    }

    pub fn target(&self, entry: &KvKey) -> Result<&ReceiptTarget, ReceiptError> {
        self.targets
            .iter()
            .find(|target| &target.entry == entry)
            .ok_or_else(|| ReceiptError::MissingTarget(entry.clone()))
    }

    pub fn revision_of(&self, receipt: EntryRevision, target: &ReceiptTarget) -> Result<EntryRevision, ReceiptError> {
        let distance = target
            .position
            .get()
            .checked_sub(self.position.get())
            .ok_or(ReceiptError::TargetBeforeReceipt(target.position.get()))?;
        Ok(receipt.checked_add(u64::from(distance))?)
    }

    pub fn write_receipt(&self, receipt: EntryRevision, entry: &KvKey) -> Result<WriteReceipt, ReceiptError> {
        let target = self.target(entry)?;
        Ok(WriteReceipt {
            lifetime: target.lifetime,
            stored_ref: target.stored_ref.clone(),
            sequence: target.sequence,
            revision: self.revision_of(receipt, target)?,
        })
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ReceiptError> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > RECEIPT_MAX_ENCODED_BYTES {
            return Err(ReceiptError::TooLarge {
                size: bytes.len(),
                max: RECEIPT_MAX_ENCODED_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ReceiptError> {
        if bytes.len() > RECEIPT_MAX_ENCODED_BYTES {
            return Err(ReceiptError::TooLarge {
                size: bytes.len(),
                max: RECEIPT_MAX_ENCODED_BYTES,
            });
        }
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReceipt {
    lifetime: LifetimeId,
    stored_ref: StoredRef,
    sequence: MutationSequence,
    revision: EntryRevision,
}

impl WriteReceipt {
    pub(crate) fn new(
        lifetime: LifetimeId,
        stored_ref: StoredRef,
        sequence: MutationSequence,
        revision: EntryRevision,
    ) -> Self {
        Self {
            lifetime,
            stored_ref,
            sequence,
            revision,
        }
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }

    pub fn stored_ref(&self) -> &StoredRef {
        &self.stored_ref
    }

    pub fn sequence(&self) -> MutationSequence {
        self.sequence
    }

    pub fn revision(&self) -> EntryRevision {
        self.revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Live,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardKind {
    Direct,
    Managed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardBody {
    v: SchemaTag<GUARD_SCHEMA_V1>,
    kind: GuardKind,
    holder: HolderId,
    owner: OwnerId,
}

impl GuardBody {
    pub(crate) fn direct(holder: HolderId, owner: OwnerId) -> Self {
        Self {
            v: SchemaTag,
            kind: GuardKind::Direct,
            holder,
            owner,
        }
    }

    pub fn holder(&self) -> HolderId {
        self.holder
    }

    pub fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{RequestWindow, UnixMillis, WriteRequest};
    use crate::position::OperationId;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn position(at: u16) -> Result<BatchPosition, Box<dyn std::error::Error>> {
        Ok(BatchPosition::try_from(at)?)
    }

    #[test]
    fn reconstructs_entry_revisions_from_receipt_positions() -> TestResult {
        let intent = WriteRequest::new(
            OperationId::from([1; 16]),
            RequestWindow::new(UnixMillis::from(1), UnixMillis::from(2)),
            HolderId::from([2; 16]),
        )
        .track("room:lobby".parse()?, "ana".parse()?, serde_json::from_str("{}")?);
        let entry = KvKey::try_from("s56.ana.AgICAgICAgICAgICAgICAg.room.lobby".to_owned())?;
        let receipt = Receipt::new(
            &intent,
            position(2)?,
            vec![ReceiptTarget::new(
                entry.clone(),
                position(3)?,
                LifetimeId::from([3; 16]),
                StoredRef::generate()?,
                MutationSequence::FIRST,
            )],
        );
        let decoded = Receipt::from_json_bytes(&receipt.to_json_bytes()?)?;
        assert_eq!(decoded, receipt);
        let written = decoded.write_receipt(EntryRevision::from(41), &entry)?;
        assert_eq!(written.revision(), EntryRevision::from(42));
        assert_eq!(written.sequence(), MutationSequence::FIRST);
        let other = KvKey::try_from("s01.bob.AgICAgICAgICAgICAgICAg.room.lobby".to_owned())?;
        assert!(matches!(
            decoded.write_receipt(EntryRevision::from(41), &other),
            Err(ReceiptError::MissingTarget(_))
        ));
        Ok(())
    }

    #[test]
    fn rejects_foreign_schema_versions() -> TestResult {
        let guard = GuardBody::direct(HolderId::from([2; 16]), OwnerId::from([4; 16]));
        let encoded = String::from_utf8(guard.to_json_bytes()?)?;
        assert!(encoded.starts_with(r#"{"v":1,"kind":"direct""#));
        assert_eq!(GuardBody::from_json_bytes(encoded.as_bytes())?, guard);
        let future = encoded.replacen(r#""v":1"#, r#""v":2"#, 1);
        assert!(GuardBody::from_json_bytes(future.as_bytes()).is_err());
        Ok(())
    }
}
