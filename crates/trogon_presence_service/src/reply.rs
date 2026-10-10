use std::fmt;

use async_nats::{HeaderMap, Message};
use serde::Serialize;
use trogon_presence::{
    BarrierError, DiffSequence, ErrorCode, GenerationEpoch, LifetimeId, Meta, MutationSequence, ShardCount, StoredRef,
    Topic, ViewShard,
};

pub const HEADER_GENERATION: &str = "Presence-Generation";
pub const HEADER_OWNER_REV: &str = "Presence-Owner-Rev";
pub const HEADER_OWNER_ID: &str = "Presence-Owner-Id";
pub const HEADER_SEQ: &str = "Presence-Seq";
pub const HEADER_PREV: &str = "Presence-Prev";
pub const HEADER_SHARD: &str = "Presence-Shard";
pub const HEADER_TOPIC: &str = "Presence-Topic";
pub const HEADER_KIND: &str = "Presence-Kind";
pub const HEADER_SNAPSHOT_ID: &str = "Presence-Snapshot-Id";
pub const HEADER_REQUEST_ID: &str = "Presence-Request-Id";
pub const HEADER_PART: &str = "Presence-Part";
pub const HEADER_PARTS: &str = "Presence-Parts";
pub const HEADER_CODE: &str = "Presence-Code";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameKind {
    Diff,
    Keepalive,
    State,
    Manifest,
    SnapshotPart,
    SnapshotEnd,
}

impl FrameKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Keepalive => "keepalive",
            Self::State => "state",
            Self::Manifest => "manifest",
            Self::SnapshotPart => "snapshot-part",
            Self::SnapshotEnd => "snapshot-end",
        }
    }
}

pub fn epoch_headers(headers: &mut HeaderMap, epoch: GenerationEpoch) {
    headers.insert(HEADER_GENERATION, epoch.generation().to_string());
    headers.insert(HEADER_OWNER_REV, epoch.epoch().acquired().to_string());
    headers.insert(HEADER_OWNER_ID, epoch.epoch().owner().to_string());
}

pub fn topic_header(topic: &Topic) -> Option<&str> {
    let raw = topic.as_str();
    raw.bytes().all(|b| (0x20..0x7f).contains(&b)).then_some(raw)
}

#[derive(Debug, Clone, Copy)]
pub struct Position {
    pub epoch: GenerationEpoch,
    pub shards: ShardCount,
    pub shard: ViewShard,
    pub seq: DiffSequence,
    pub prev: Option<DiffSequence>,
}

impl Position {
    pub fn headers(&self, topic: &Topic, kind: FrameKind) -> HeaderMap {
        let mut headers = HeaderMap::new();
        epoch_headers(&mut headers, self.epoch);
        headers.insert(HEADER_SEQ, self.seq.to_string());
        if let Some(prev) = self.prev {
            headers.insert(HEADER_PREV, prev.to_string());
        }
        headers.insert(HEADER_SHARD, self.shards.token(self.shard));
        headers.insert(HEADER_KIND, kind.as_str());
        if let Some(raw) = topic_header(topic) {
            headers.insert(HEADER_TOPIC, raw);
        }
        headers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyCode {
    Ok,
    NotOwner,
    InvalidRequest,
    InvalidTopic,
    InvalidKey,
    InvalidMeta,
    KeyTooLong,
    TopicTooLong,
    HolderTooLong,
    Gone,
    NotFound,
    AlreadyTracked,
    Conflict,
    OperationConflict,
    SequenceConflict,
    RetryExpired,
    HolderLimit,
    TopicLimit,
    IdentityCapacityExceeded,
    MetaTooLarge,
    Unavailable,
    BarrierExpired,
    GenerationChanged,
    NotReady,
    Overloaded,
    SnapshotTooLarge,
    HookRejected,
    HookUnavailable,
}

impl ReplyCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotOwner => "not_owner",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidTopic => "invalid_topic",
            Self::InvalidKey => "invalid_key",
            Self::InvalidMeta => "invalid_meta",
            Self::KeyTooLong => "key_too_long",
            Self::TopicTooLong => "topic_too_long",
            Self::HolderTooLong => "holder_too_long",
            Self::Gone => "gone",
            Self::NotFound => "not_found",
            Self::AlreadyTracked => "already_tracked",
            Self::Conflict => "conflict",
            Self::OperationConflict => "operation_conflict",
            Self::SequenceConflict => "sequence_conflict",
            Self::RetryExpired => "retry_expired",
            Self::HolderLimit => "holder_limit",
            Self::TopicLimit => "topic_limit",
            Self::IdentityCapacityExceeded => "identity_capacity_exceeded",
            Self::MetaTooLarge => "meta_too_large",
            Self::Unavailable => "unavailable",
            Self::BarrierExpired => "barrier_expired",
            Self::GenerationChanged => "generation_changed",
            Self::NotReady => "not_ready",
            Self::Overloaded => "overloaded",
            Self::SnapshotTooLarge => "snapshot_too_large",
            Self::HookRejected => "hook_rejected",
            Self::HookUnavailable => "hook_unavailable",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::NotOwner
                | Self::NotReady
                | Self::Unavailable
                | Self::Overloaded
                | Self::HookUnavailable
                | Self::BarrierExpired
        )
    }
}

impl From<ErrorCode> for ReplyCode {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::InvalidTopic => Self::InvalidTopic,
            ErrorCode::InvalidKey => Self::InvalidKey,
            ErrorCode::TopicTooLong => Self::TopicTooLong,
            ErrorCode::KeyTooLong => Self::KeyTooLong,
            ErrorCode::HolderTooLong => Self::HolderTooLong,
            ErrorCode::MetaTooLarge => Self::MetaTooLarge,
            ErrorCode::InvalidRequest => Self::InvalidRequest,
            ErrorCode::AlreadyTracked => Self::AlreadyTracked,
            ErrorCode::Gone => Self::Gone,
            ErrorCode::Conflict => Self::Conflict,
            ErrorCode::OperationConflict => Self::OperationConflict,
            ErrorCode::SequenceConflict => Self::SequenceConflict,
            ErrorCode::RetryExpired => Self::RetryExpired,
            ErrorCode::HolderLimit => Self::HolderLimit,
            ErrorCode::TopicLimit => Self::TopicLimit,
            ErrorCode::IdentityCapacityExceeded => Self::IdentityCapacityExceeded,
            ErrorCode::NotReady => Self::NotReady,
            ErrorCode::Unavailable => Self::Unavailable,
            ErrorCode::Overloaded => Self::Overloaded,
            ErrorCode::GenerationChanged => Self::GenerationChanged,
            ErrorCode::BarrierExpired => Self::BarrierExpired,
            ErrorCode::SnapshotTooLarge => Self::SnapshotTooLarge,
            ErrorCode::HookRejected => Self::HookRejected,
            ErrorCode::HookUnavailable => Self::HookUnavailable,
        }
    }
}

impl From<&BarrierError> for ReplyCode {
    fn from(err: &BarrierError) -> Self {
        match err {
            BarrierError::Expired { .. } => Self::BarrierExpired,
            BarrierError::GenerationChanged { .. } => Self::GenerationChanged,
            BarrierError::NotReady => Self::NotReady,
            BarrierError::Unavailable { .. } => Self::Unavailable,
            BarrierError::Overloaded => Self::Overloaded,
            BarrierError::WrongTopic { .. } => Self::InvalidRequest,
        }
    }
}

impl From<BarrierError> for ErrorReply {
    fn from(err: BarrierError) -> Self {
        Self::new(ReplyCode::from(&err)).detail(err)
    }
}

impl fmt::Display for ReplyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorReply {
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shard: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phx_ref: Option<StoredRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<Meta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lifetime: Option<LifetimeId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation_seq: Option<MutationSequence>,
    retryable: bool,
    outcome_unknown: bool,
    #[serde(skip)]
    code: ReplyCode,
}

impl ErrorReply {
    pub fn new(code: ReplyCode) -> Self {
        Self {
            error: code.as_str(),
            detail: None,
            shard: None,
            phx_ref: None,
            meta: None,
            lifetime: None,
            mutation_seq: None,
            retryable: code.retryable(),
            outcome_unknown: false,
            code,
        }
    }

    pub fn outcome_unknown(self) -> Self {
        Self {
            outcome_unknown: true,
            retryable: true,
            ..self
        }
    }

    pub fn entry(self, lifetime: LifetimeId, mutation_seq: MutationSequence) -> Self {
        Self {
            lifetime: Some(lifetime),
            mutation_seq: Some(mutation_seq),
            ..self
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    pub fn is_outcome_unknown(&self) -> bool {
        self.outcome_unknown
    }

    pub fn detail(self, detail: impl fmt::Display) -> Self {
        Self {
            detail: Some(detail.to_string()),
            ..self
        }
    }

    pub fn not_owner(shards: ShardCount, shard: ViewShard) -> Self {
        Self {
            shard: Some(shards.token(shard)),
            ..Self::new(ReplyCode::NotOwner)
        }
    }

    pub fn current(self, phx_ref: StoredRef, meta: Option<Meta>) -> Self {
        Self {
            phx_ref: Some(phx_ref),
            meta,
            ..self
        }
    }

    pub fn code(&self) -> ReplyCode {
        self.code
    }
}

#[derive(Debug)]
pub struct Response {
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Response {
    pub fn ok(body: Vec<u8>) -> Self {
        Self::with_code(ReplyCode::Ok, HeaderMap::new(), body)
    }

    pub fn ok_with(headers: HeaderMap, body: Vec<u8>) -> Self {
        Self::with_code(ReplyCode::Ok, headers, body)
    }

    pub fn ok_json<T: Serialize>(body: &T) -> Self {
        match serde_json::to_vec(body) {
            Ok(body) => Self::ok(body),
            Err(err) => Self::error(ErrorReply::new(ReplyCode::Unavailable).detail(err)),
        }
    }

    pub fn error(reply: ErrorReply) -> Self {
        let mut headers = HeaderMap::new();
        if let Some(shard) = &reply.shard {
            headers.insert(HEADER_SHARD, shard.as_str());
        }
        let body = serde_json::to_vec(&reply).unwrap_or_default();
        Self::with_code(reply.code, headers, body)
    }

    fn with_code(code: ReplyCode, mut headers: HeaderMap, body: Vec<u8>) -> Self {
        headers.insert(HEADER_CODE, code.as_str());
        Self { headers, body }
    }

    #[cfg(test)]
    pub(crate) fn body(&self) -> &[u8] {
        &self.body
    }

    pub async fn send(self, client: &async_nats::Client, request: &Message) {
        let Some(reply) = request.reply.clone() else {
            return;
        };
        self.send_to(client, reply).await;
    }

    pub async fn send_to(self, client: &async_nats::Client, reply: async_nats::Subject) {
        if let Err(err) = client.publish_with_headers(reply, self.headers, self.body.into()).await {
            tracing::warn!(%err, "could not publish a presence reply");
        }
    }
}

impl From<ErrorReply> for Response {
    fn from(reply: ErrorReply) -> Self {
        Self::error(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn error_reply_shapes() -> TestResult {
        let shards = ShardCount::DEFAULT;
        let reply = ErrorReply::not_owner(shards, ViewShard::from(shards.shard(56)?));
        assert_eq!(
            serde_json::to_string(&reply)?,
            r#"{"error":"not_owner","shard":"s56","retryable":true,"outcome_unknown":false}"#
        );
        let gone = ErrorReply::new(ReplyCode::Gone).detail("entry expired");
        assert_eq!(
            serde_json::to_string(&gone)?,
            r#"{"error":"gone","detail":"entry expired","retryable":false,"outcome_unknown":false}"#
        );
        let unknown = ErrorReply::new(ReplyCode::Unavailable).outcome_unknown();
        assert_eq!(
            serde_json::to_string(&unknown)?,
            r#"{"error":"unavailable","retryable":true,"outcome_unknown":true}"#
        );
        assert!(!ErrorReply::new(ReplyCode::HolderLimit).is_retryable());
        assert!(ErrorReply::new(ReplyCode::Overloaded).is_retryable());
        Ok(())
    }

    #[test]
    fn every_core_error_code_keeps_its_wire_name() {
        for code in [
            ErrorCode::InvalidTopic,
            ErrorCode::InvalidKey,
            ErrorCode::TopicTooLong,
            ErrorCode::KeyTooLong,
            ErrorCode::HolderTooLong,
            ErrorCode::MetaTooLarge,
            ErrorCode::InvalidRequest,
            ErrorCode::AlreadyTracked,
            ErrorCode::Gone,
            ErrorCode::Conflict,
            ErrorCode::OperationConflict,
            ErrorCode::SequenceConflict,
            ErrorCode::RetryExpired,
            ErrorCode::HolderLimit,
            ErrorCode::TopicLimit,
            ErrorCode::IdentityCapacityExceeded,
            ErrorCode::NotReady,
            ErrorCode::Unavailable,
            ErrorCode::Overloaded,
            ErrorCode::GenerationChanged,
            ErrorCode::BarrierExpired,
            ErrorCode::SnapshotTooLarge,
            ErrorCode::HookRejected,
            ErrorCode::HookUnavailable,
        ] {
            assert_eq!(ReplyCode::from(code).as_str(), code.as_str());
            assert_eq!(ReplyCode::from(code).retryable(), code.retryable());
        }
    }

    #[test]
    fn formats_headers() -> TestResult {
        let generation = trogon_presence::StreamGeneration::from([1; 16]);
        let owner = trogon_presence::OwnerId::from([2; 16]);
        let epoch = GenerationEpoch::new(
            generation,
            trogon_presence::OwnerEpoch::new(trogon_presence::EntryRevision::from(7), owner),
        );
        let position = Position {
            epoch,
            shards: ShardCount::DEFAULT,
            shard: ViewShard::from(ShardCount::DEFAULT.shard(56)?),
            seq: DiffSequence::from(4),
            prev: Some(DiffSequence::from(3)),
        };
        let headers = position.headers(&"room:lobby".parse()?, FrameKind::Diff);
        let header = |name: &str| headers.get(name).map(|value| value.as_str().to_owned());
        assert_eq!(header(HEADER_GENERATION), Some(generation.to_string()));
        assert_eq!(header(HEADER_OWNER_REV).as_deref(), Some("7"));
        assert_eq!(header(HEADER_OWNER_ID), Some(owner.to_string()));
        assert_eq!(header(HEADER_SEQ).as_deref(), Some("4"));
        assert_eq!(header(HEADER_PREV).as_deref(), Some("3"));
        assert_eq!(header("Presence-Rev"), None);
        assert_eq!(header("Presence-Epoch"), None);
        assert_eq!(topic_header(&"room:lobby".parse()?), Some("room:lobby"));
        assert_eq!(topic_header(&"room:a\r\nb".parse()?), None);
        assert_eq!(topic_header(&"room:jos\u{e9}".parse()?), None);
        Ok(())
    }
}
