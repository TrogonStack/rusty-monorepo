use std::fmt;
use std::str::FromStr;

use async_nats::{HeaderMap, Message};
use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use trogon_presence::watch::ViewCursor;
use trogon_presence::{
    DiffSequence, EntryRevision, GenerationEpoch, OwnerEpoch, OwnerId, Presences, RequestId, SnapshotId,
    StreamGeneration,
};

use super::limits::SnapshotLimits;
use crate::reply::{
    epoch_headers, FrameKind, HEADER_GENERATION, HEADER_KIND, HEADER_OWNER_ID, HEADER_OWNER_REV, HEADER_PART,
    HEADER_PARTS, HEADER_REQUEST_ID, HEADER_SEQ, HEADER_SNAPSHOT_ID,
};
use crate::subjects::SnapshotReplySubject;

const DIGEST_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotBytes(u64);

impl SnapshotBytes {
    pub fn get(self) -> u64 {
        self.0
    }

    pub fn reservation(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

impl From<usize> for SnapshotBytes {
    fn from(value: usize) -> Self {
        Self(u64::try_from(value).unwrap_or(u64::MAX))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartCount(u32);

impl PartCount {
    pub fn get(self) -> u32 {
        self.0
    }

    fn slots(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

impl From<u32> for PartCount {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for PartCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartIndex(u32);

impl PartIndex {
    pub const FIRST: Self = Self(1);

    pub fn get(self) -> u32 {
        self.0
    }

    fn slot(self, count: PartCount) -> Option<usize> {
        (self.0 >= 1 && self.0 <= count.0).then(|| usize::try_from(self.0 - 1).unwrap_or(usize::MAX))
    }
}

impl From<u32> for PartIndex {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for PartIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotDigest([u8; DIGEST_BYTES]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("snapshot digest must be {DIGEST_BYTES} bytes of lowercase hex, got {0:?}")]
pub struct SnapshotDigestError(String);

impl SnapshotDigest {
    pub fn of<'a>(parts: impl IntoIterator<Item = &'a Bytes>) -> Self {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part);
        }
        Self(hasher.finalize().into())
    }
}

impl From<[u8; DIGEST_BYTES]> for SnapshotDigest {
    fn from(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for SnapshotDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for SnapshotDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SnapshotDigest({self})")
    }
}

impl FromStr for SnapshotDigest {
    type Err = SnapshotDigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || SnapshotDigestError(s.to_owned());
        if s.len() != DIGEST_BYTES * 2 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(invalid());
        }
        let mut bytes = [0u8; DIGEST_BYTES];
        for (slot, pair) in bytes.iter_mut().zip(s.as_bytes().chunks(2)) {
            let text = std::str::from_utf8(pair).map_err(|_| invalid())?;
            *slot = u8::from_str_radix(text, 16).map_err(|_| invalid())?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for SnapshotDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SnapshotDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotIdentity {
    request: RequestId,
    snapshot: SnapshotId,
    epoch: GenerationEpoch,
    seq: DiffSequence,
}

impl SnapshotIdentity {
    pub fn new(request: RequestId, snapshot: SnapshotId, epoch: GenerationEpoch, seq: DiffSequence) -> Self {
        Self {
            request,
            snapshot,
            epoch,
            seq,
        }
    }

    pub fn request(&self) -> RequestId {
        self.request
    }

    pub fn snapshot(&self) -> SnapshotId {
        self.snapshot
    }

    pub fn epoch(&self) -> GenerationEpoch {
        self.epoch
    }

    pub fn seq(&self) -> DiffSequence {
        self.seq
    }

    pub fn cursor(&self) -> ViewCursor {
        ViewCursor::Service {
            epoch: self.epoch,
            seq: self.seq,
        }
    }

    fn headers(&self, kind: FrameKind) -> HeaderMap {
        let mut headers = HeaderMap::new();
        epoch_headers(&mut headers, self.epoch);
        headers.insert(HEADER_SEQ, self.seq.to_string());
        headers.insert(HEADER_SNAPSHOT_ID, self.snapshot.to_string());
        headers.insert(HEADER_REQUEST_ID, self.request.to_string());
        headers.insert(HEADER_KIND, kind.as_str());
        headers
    }

    fn from_headers(headers: &HeaderMap) -> Result<Self, SnapshotFrameError> {
        let generation: StreamGeneration = parse_header(headers, HEADER_GENERATION)?;
        let owner: OwnerId = parse_header(headers, HEADER_OWNER_ID)?;
        let acquired: u64 = parse_header(headers, HEADER_OWNER_REV)?;
        let seq: u64 = parse_header(headers, HEADER_SEQ)?;
        Ok(Self {
            request: parse_header(headers, HEADER_REQUEST_ID)?,
            snapshot: parse_header(headers, HEADER_SNAPSHOT_ID)?,
            epoch: GenerationEpoch::new(generation, OwnerEpoch::new(EntryRevision::from(acquired), owner)),
            seq: DiffSequence::from(seq),
        })
    }
}

fn parse_header<T: FromStr>(headers: &HeaderMap, name: &'static str) -> Result<T, SnapshotFrameError> {
    headers
        .get(name)
        .and_then(|value| value.as_str().parse().ok())
        .ok_or(SnapshotFrameError::Header(name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    request_id: RequestId,
    snapshot_id: SnapshotId,
    generation: StreamGeneration,
    owner_epoch: OwnerEpoch,
    seq: DiffSequence,
    total_bytes: SnapshotBytes,
    parts: PartCount,
    sha256: SnapshotDigest,
}

impl SnapshotManifest {
    pub fn identity(&self) -> SnapshotIdentity {
        SnapshotIdentity {
            request: self.request_id,
            snapshot: self.snapshot_id,
            epoch: GenerationEpoch::new(self.generation, self.owner_epoch),
            seq: self.seq,
        }
    }

    pub fn total_bytes(&self) -> SnapshotBytes {
        self.total_bytes
    }

    pub fn parts(&self) -> PartCount {
        self.parts
    }

    pub fn digest(&self) -> SnapshotDigest {
        self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("snapshot of {bytes} bytes exceeds the {max} byte cap")]
    TooLarge { bytes: usize, max: usize },
    #[error("snapshot needs {parts} parts, more than the {max} part cap")]
    TooManyParts { parts: usize, max: u32 },
    #[error("payload budget of {budget} bytes leaves no room after {headers} header bytes")]
    NoRoom { budget: usize, headers: usize },
    #[error("could not encode the snapshot: {0}")]
    Encode(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedSnapshot {
    manifest: SnapshotManifest,
    parts: Vec<Bytes>,
}

impl CapturedSnapshot {
    pub fn capture(
        identity: SnapshotIdentity,
        state: &Presences,
        limits: SnapshotLimits,
    ) -> Result<Self, CaptureError> {
        let body = serde_json::to_vec(state).map_err(|err| CaptureError::Encode(err.to_string()))?;
        Self::from_json(identity, Bytes::from(body), limits)
    }

    pub fn from_json(identity: SnapshotIdentity, body: Bytes, limits: SnapshotLimits) -> Result<Self, CaptureError> {
        let max = limits.max_bytes().get();
        if body.len() > max {
            return Err(CaptureError::TooLarge { bytes: body.len(), max });
        }
        let mut widest = identity.headers(FrameKind::SnapshotPart);
        widest.insert(HEADER_PART, limits.max_parts().get().to_string());
        let headers = header_wire_len(&widest);
        let budget = limits.payload().get();
        let room = budget.saturating_sub(headers);
        if room == 0 {
            return Err(CaptureError::NoRoom { budget, headers });
        }
        let parts: Vec<Bytes> = if body.is_empty() {
            vec![body.clone()]
        } else {
            (0..body.len())
                .step_by(room)
                .map(|start| body.slice(start..(start + room).min(body.len())))
                .collect()
        };
        let count = u32::try_from(parts.len())
            .ok()
            .filter(|count| *count <= limits.max_parts().get())
            .ok_or(CaptureError::TooManyParts {
                parts: parts.len(),
                max: limits.max_parts().get(),
            })?;
        let epoch = identity.epoch;
        let manifest = SnapshotManifest {
            request_id: identity.request,
            snapshot_id: identity.snapshot,
            generation: epoch.generation(),
            owner_epoch: epoch.epoch(),
            seq: identity.seq,
            total_bytes: SnapshotBytes::from(body.len()),
            parts: PartCount(count),
            sha256: SnapshotDigest::of(&parts),
        };
        Ok(Self { manifest, parts })
    }

    pub fn manifest(&self) -> &SnapshotManifest {
        &self.manifest
    }

    pub fn frames(&self) -> Vec<SnapshotFrame> {
        let identity = self.manifest.identity();
        let mut frames: Vec<SnapshotFrame> = self
            .parts
            .iter()
            .zip(1u32..)
            .map(|(bytes, index)| SnapshotFrame::Part {
                identity,
                index: PartIndex(index),
                bytes: bytes.clone(),
            })
            .collect();
        frames.push(SnapshotFrame::End {
            identity,
            parts: self.manifest.parts,
        });
        frames
    }

    pub async fn deliver(
        &self,
        client: &async_nats::Client,
        reply: &SnapshotReplySubject,
    ) -> Result<(), async_nats::PublishError> {
        for frame in self.frames() {
            frame.publish(client, reply).await?;
        }
        Ok(())
    }
}

pub fn header_wire_len(headers: &HeaderMap) -> usize {
    let mut len = "NATS/1.0\r\n".len() + "\r\n".len();
    for (name, values) in headers.iter() {
        for value in values {
            len += AsRef::<str>::as_ref(name).len() + ": ".len() + value.as_str().len() + "\r\n".len();
        }
    }
    len
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotFrameError {
    #[error("snapshot frame has no headers")]
    NoHeaders,
    #[error("snapshot frame header {0} is missing or malformed")]
    Header(&'static str),
    #[error("snapshot frame kind is not a snapshot part or end")]
    Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotFrame {
    Part {
        identity: SnapshotIdentity,
        index: PartIndex,
        bytes: Bytes,
    },
    End {
        identity: SnapshotIdentity,
        parts: PartCount,
    },
}

impl SnapshotFrame {
    pub fn identity(&self) -> &SnapshotIdentity {
        match self {
            Self::Part { identity, .. } | Self::End { identity, .. } => identity,
        }
    }

    pub fn headers(&self) -> HeaderMap {
        match self {
            Self::Part { identity, index, .. } => {
                let mut headers = identity.headers(FrameKind::SnapshotPart);
                headers.insert(HEADER_PART, index.to_string());
                headers
            }
            Self::End { identity, parts } => {
                let mut headers = identity.headers(FrameKind::SnapshotEnd);
                headers.insert(HEADER_PARTS, parts.to_string());
                headers
            }
        }
    }

    pub fn payload(&self) -> Bytes {
        match self {
            Self::Part { bytes, .. } => bytes.clone(),
            Self::End { .. } => Bytes::new(),
        }
    }

    pub async fn publish(
        &self,
        client: &async_nats::Client,
        reply: &SnapshotReplySubject,
    ) -> Result<(), async_nats::PublishError> {
        client
            .publish_with_headers(reply.as_str().to_owned(), self.headers(), self.payload())
            .await
    }
}

impl TryFrom<&Message> for SnapshotFrame {
    type Error = SnapshotFrameError;

    fn try_from(message: &Message) -> Result<Self, Self::Error> {
        let headers = message.headers.as_ref().ok_or(SnapshotFrameError::NoHeaders)?;
        let identity = SnapshotIdentity::from_headers(headers)?;
        let kind = headers.get(HEADER_KIND).map(|value| value.as_str());
        if kind == Some(FrameKind::SnapshotPart.as_str()) {
            let index: u32 = parse_header(headers, HEADER_PART)?;
            Ok(Self::Part {
                identity,
                index: PartIndex(index),
                bytes: message.payload.clone(),
            })
        } else if kind == Some(FrameKind::SnapshotEnd.as_str()) {
            let parts: u32 = parse_header(headers, HEADER_PARTS)?;
            Ok(Self::End {
                identity,
                parts: PartCount(parts),
            })
        } else {
            Err(SnapshotFrameError::Kind)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssemblyError {
    #[error("manifest advertises {bytes} bytes, over the {max} byte cap")]
    TooLarge { bytes: u64, max: usize },
    #[error("manifest advertises {parts} parts, outside 1 to {max}")]
    PartCount { parts: u32, max: u32 },
    #[error("a frame carries another request, snapshot or cursor")]
    MixedIdentity,
    #[error("part {0} arrived twice with different bytes")]
    ConflictingDuplicate(PartIndex),
    #[error("part {0} is outside the advertised range")]
    OutOfRange(PartIndex),
    #[error("parts exceed the advertised byte total")]
    Overflow,
    #[error("parts fall short of the advertised byte total")]
    Truncated,
    #[error("end advertises {0} parts, unlike the manifest")]
    EndMismatch(PartCount),
    #[error("assembled bytes do not match the manifest digest")]
    DigestMismatch,
    #[error("assembled snapshot is not a presence map: {0}")]
    Decode(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssemblyProgress {
    Pending,
    Complete(Presences),
}

#[derive(Debug)]
pub struct Assembly {
    manifest: SnapshotManifest,
    parts: Vec<Option<Bytes>>,
    received: u64,
    ended: bool,
}

impl Assembly {
    pub fn begin(manifest: SnapshotManifest, limits: SnapshotLimits) -> Result<Self, AssemblyError> {
        let max = limits.max_bytes().get();
        if manifest.total_bytes.reservation() > max {
            return Err(AssemblyError::TooLarge {
                bytes: manifest.total_bytes.get(),
                max,
            });
        }
        let max_parts = limits.max_parts().get();
        if manifest.parts.0 == 0 || manifest.parts.0 > max_parts {
            return Err(AssemblyError::PartCount {
                parts: manifest.parts.0,
                max: max_parts,
            });
        }
        Ok(Self {
            manifest,
            parts: vec![None; manifest.parts.slots()],
            received: 0,
            ended: false,
        })
    }

    pub fn manifest(&self) -> &SnapshotManifest {
        &self.manifest
    }

    pub fn accept(&mut self, frame: SnapshotFrame) -> Result<AssemblyProgress, AssemblyError> {
        if *frame.identity() != self.manifest.identity() {
            return Err(AssemblyError::MixedIdentity);
        }
        match frame {
            SnapshotFrame::Part { index, bytes, .. } => {
                let slot = index
                    .slot(self.manifest.parts)
                    .ok_or(AssemblyError::OutOfRange(index))?;
                match &self.parts[slot] {
                    Some(seen) if *seen == bytes => {}
                    Some(_) => return Err(AssemblyError::ConflictingDuplicate(index)),
                    None => {
                        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        self.received = self.received.saturating_add(len);
                        if self.received > self.manifest.total_bytes.get() {
                            return Err(AssemblyError::Overflow);
                        }
                        self.parts[slot] = Some(bytes);
                    }
                }
            }
            SnapshotFrame::End { parts, .. } => {
                if parts != self.manifest.parts {
                    return Err(AssemblyError::EndMismatch(parts));
                }
                self.ended = true;
            }
        }
        self.finish()
    }

    fn finish(&self) -> Result<AssemblyProgress, AssemblyError> {
        if !self.ended || self.parts.iter().any(Option::is_none) {
            return Ok(AssemblyProgress::Pending);
        }
        if self.received != self.manifest.total_bytes.get() {
            return Err(AssemblyError::Truncated);
        }
        let parts: Vec<&Bytes> = self.parts.iter().flatten().collect();
        if SnapshotDigest::of(parts.iter().copied()) != self.manifest.sha256 {
            return Err(AssemblyError::DigestMismatch);
        }
        let joined: Vec<u8> = parts.into_iter().flat_map(|part| part.iter().copied()).collect();
        serde_json::from_slice(&joined)
            .map(AssemblyProgress::Complete)
            .map_err(|err| AssemblyError::Decode(err.to_string()))
    }
}
