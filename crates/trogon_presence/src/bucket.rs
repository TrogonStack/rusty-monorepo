use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_nats::jetstream::context::{CreateStreamError, DeleteStreamError, DeleteStreamErrorKind};
use async_nats::jetstream::stream::{self, DiscardPolicy, RetentionPolicy};
use async_nats::jetstream::ErrorCode;
use async_nats::{jetstream, Subject};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use bytes::Bytes;
use sha2::{Digest, Sha256};

use crate::batch::{AtomicBatch, BatchError, BatchOutcome, BatchPublishError, BatchRecord, Expected};
use crate::config::{BucketName, PresenceConfig, ProvisionOptions};
use crate::constants::{
    BATCH_MAX_MESSAGES, BUCKET_MAX_AGE_SECS, BUCKET_MAX_BYTES, BUCKET_MAX_MESSAGE_BYTES, BUCKET_SCHEMA_V1,
    FINGERPRINT_METADATA_KEY, GENERATION_METADATA_KEY, SCHEMA_METADATA_KEY, SHARD_COUNT_METADATA_KEY,
    TOKEN_WIDTH_METADATA_KEY, WRITER_MODE_METADATA_KEY,
};
use crate::entropy::EntropyError;
use crate::position::{BatchId, StreamGeneration};
use crate::shard::ShardCount;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BucketField {
    Subjects,
    Retention,
    History,
    Storage,
    Replicas,
    Discard,
    MaxBytes,
    MaxAge,
    MaxMessageSize,
    MessageTtl,
    MarkerTtl,
    Rollup,
    DenyDelete,
    AtomicPublish,
    DirectGet,
    Schema,
    Generation,
    ShardCount,
    TokenWidth,
    WriterMode,
    Fingerprint,
}

impl fmt::Display for BucketField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Subjects => "subjects",
            Self::Retention => "retention",
            Self::History => "history",
            Self::Storage => "storage",
            Self::Replicas => "replicas",
            Self::Discard => "discard policy",
            Self::MaxBytes => "max bytes",
            Self::MaxAge => "max age",
            Self::MaxMessageSize => "max message size",
            Self::MessageTtl => "per-message ttl",
            Self::MarkerTtl => "subject delete marker ttl",
            Self::Rollup => "rollup",
            Self::DenyDelete => "deny delete",
            Self::AtomicPublish => "atomic publish",
            Self::DirectGet => "direct get",
            Self::Schema => "schema",
            Self::Generation => "generation",
            Self::ShardCount => "shard count",
            Self::TokenWidth => "token width",
            Self::WriterMode => "writer mode",
            Self::Fingerprint => "fingerprint",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WriterMode {
    #[default]
    Direct,
    Managed,
}

impl WriterMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Managed => "managed",
        }
    }
}

impl std::fmt::Display for WriterMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WriterMode {
    type Err = BucketField;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "direct" => Ok(Self::Direct),
            "managed" => Ok(Self::Managed),
            _ => Err(BucketField::WriterMode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BucketSchema(&'static str);

impl BucketSchema {
    pub const V1: Self = Self(BUCKET_SCHEMA_V1);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenWidth(usize);

impl From<ShardCount> for TokenWidth {
    fn from(shards: ShardCount) -> Self {
        Self(shards.token_width())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BucketMetadata {
    schema: BucketSchema,
    generation: StreamGeneration,
    shard_count: ShardCount,
    token_width: TokenWidth,
    writer_mode: WriterMode,
}

impl BucketMetadata {
    pub fn new(generation: StreamGeneration, config: &PresenceConfig) -> Self {
        Self {
            schema: BucketSchema::V1,
            generation,
            shard_count: config.shards(),
            token_width: TokenWidth::from(config.shards()),
            writer_mode: config.writer_mode(),
        }
    }

    pub fn generation(&self) -> StreamGeneration {
        self.generation
    }

    pub fn writer_mode(&self) -> WriterMode {
        self.writer_mode
    }

    pub fn to_map(&self) -> HashMap<String, String> {
        HashMap::from([
            (SCHEMA_METADATA_KEY.to_owned(), self.schema.0.to_owned()),
            (GENERATION_METADATA_KEY.to_owned(), self.generation.to_string()),
            (SHARD_COUNT_METADATA_KEY.to_owned(), self.shard_count.get().to_string()),
            (TOKEN_WIDTH_METADATA_KEY.to_owned(), self.token_width.0.to_string()),
            (
                WRITER_MODE_METADATA_KEY.to_owned(),
                self.writer_mode.as_str().to_owned(),
            ),
        ])
    }

    pub fn from_map(map: &HashMap<String, String>) -> Result<Self, BucketField> {
        let field = |key: &str, field: BucketField| map.get(key).ok_or(field);
        let schema = field(SCHEMA_METADATA_KEY, BucketField::Schema)?;
        if schema != BucketSchema::V1.0 {
            return Err(BucketField::Schema);
        }
        let generation = field(GENERATION_METADATA_KEY, BucketField::Generation)?
            .parse()
            .map_err(|_| BucketField::Generation)?;
        let shard_count = field(SHARD_COUNT_METADATA_KEY, BucketField::ShardCount)?
            .parse::<u16>()
            .ok()
            .and_then(|count| ShardCount::try_from(count).ok())
            .ok_or(BucketField::ShardCount)?;
        let token_width = field(TOKEN_WIDTH_METADATA_KEY, BucketField::TokenWidth)?
            .parse::<usize>()
            .map(TokenWidth)
            .map_err(|_| BucketField::TokenWidth)?;
        if token_width != TokenWidth::from(shard_count) {
            return Err(BucketField::TokenWidth);
        }
        let writer_mode = field(WRITER_MODE_METADATA_KEY, BucketField::WriterMode)?.parse()?;
        Ok(Self {
            schema: BucketSchema::V1,
            generation,
            shard_count,
            token_width,
            writer_mode,
        })
    }

    pub fn check_against(&self, config: &PresenceConfig) -> Result<(), BucketField> {
        let expected = Self::new(self.generation, config);
        if self.shard_count != expected.shard_count {
            return Err(BucketField::ShardCount);
        }
        if self.token_width != expected.token_width {
            return Err(BucketField::TokenWidth);
        }
        if self.writer_mode != expected.writer_mode {
            return Err(BucketField::WriterMode);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamIdentity {
    name: String,
    created_nanos: i128,
}

impl StreamIdentity {
    pub fn of(info: &stream::Info) -> Self {
        Self {
            name: info.config.name.clone(),
            created_nanos: info.created.unix_timestamp_nanos(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamFingerprint([u8; 32]);

impl StreamFingerprint {
    pub fn of(identities: &[StreamIdentity]) -> Self {
        let mut hasher = Sha256::new();
        for identity in identities {
            hasher.update(identity.name.as_bytes());
            hasher.update([0]);
            hasher.update(identity.created_nanos.to_string().as_bytes());
            hasher.update([0]);
        }
        Self(hasher.finalize().into())
    }

    pub fn recorded(map: &HashMap<String, String>) -> Option<Self> {
        map.get(FINGERPRINT_METADATA_KEY)?.parse().ok()
    }
}

impl FromStr for StreamFingerprint {
    type Err = BucketField;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        URL_SAFE_NO_PAD
            .decode(s)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .map(Self)
            .ok_or(BucketField::Fingerprint)
    }
}

impl fmt::Display for StreamFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for StreamFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StreamFingerprint({self})")
    }
}

pub(crate) fn data_stream_config(
    config: &PresenceConfig,
    options: &ProvisionOptions,
    metadata: &BucketMetadata,
) -> stream::Config {
    let bucket = config.bucket();
    stream::Config {
        name: bucket.stream_name(),
        subjects: vec![bucket.subjects_filter()],
        retention: RetentionPolicy::Limits,
        max_messages_per_subject: 1,
        max_bytes: BUCKET_MAX_BYTES,
        max_age: data_max_age(config),
        max_message_size: BUCKET_MAX_MESSAGE_BYTES,
        storage: options.storage,
        num_replicas: usize::from(options.replicas.get()),
        allow_rollup: true,
        deny_delete: true,
        allow_direct: false,
        discard: DiscardPolicy::New,
        allow_message_ttl: true,
        subject_delete_marker_ttl: Some(config.marker_ttl().get()),
        allow_atomic_publish: true,
        metadata: metadata.to_map(),
        ..Default::default()
    }
}

fn data_max_age(config: &PresenceConfig) -> Duration {
    Duration::from_secs(BUCKET_MAX_AGE_SECS).max(config.marker_ttl().get() * 2)
}

pub(crate) fn check_settings(found: &stream::Config, config: &PresenceConfig) -> Result<(), BucketField> {
    let bucket = config.bucket();
    let checks = [
        (found.subjects == [bucket.subjects_filter()], BucketField::Subjects),
        (found.retention == RetentionPolicy::Limits, BucketField::Retention),
        (found.max_messages_per_subject == 1, BucketField::History),
        (found.discard == DiscardPolicy::New, BucketField::Discard),
        (found.max_bytes == BUCKET_MAX_BYTES, BucketField::MaxBytes),
        (found.max_age == data_max_age(config), BucketField::MaxAge),
        (
            found.max_message_size == BUCKET_MAX_MESSAGE_BYTES,
            BucketField::MaxMessageSize,
        ),
        (found.allow_message_ttl, BucketField::MessageTtl),
        (
            found.subject_delete_marker_ttl == Some(config.marker_ttl().get()),
            BucketField::MarkerTtl,
        ),
        (found.allow_rollup, BucketField::Rollup),
        (found.deny_delete, BucketField::DenyDelete),
        (found.allow_atomic_publish, BucketField::AtomicPublish),
    ];
    match checks.into_iter().find(|(ok, _)| !ok) {
        Some((_, field)) => Err(field),
        None => Ok(()),
    }
}

pub(crate) fn check_placement(found: &stream::Config, options: &ProvisionOptions) -> Result<(), BucketField> {
    if found.storage != options.storage {
        return Err(BucketField::Storage);
    }
    if found.num_replicas != usize::from(options.replicas.get()) {
        return Err(BucketField::Replicas);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("could not mint a probe id: {0}")]
    Entropy(#[from] EntropyError),
    #[error("could not create the probe stream: {0}")]
    Create(#[source] CreateStreamError),
    #[error("could not stage the probe batch: {0}")]
    Stage(#[from] BatchError),
    #[error("could not publish the probe batch: {0}")]
    Publish(#[from] BatchPublishError),
    #[error("server refused the probe batch with code {code}: {description}")]
    Refused { code: u64, description: String },
    #[error("the probe batch outcome is unknown")]
    Unknown,
    #[error("the probe batch committed {found} of {expected} records")]
    Partial { expected: usize, found: u16 },
    #[error("could not remove the leftover probe stream {stream}: {source}")]
    Leftover { stream: String, source: DeleteStreamError },
    #[error("could not delete the probe stream {stream}: {source}")]
    Cleanup { stream: String, source: DeleteStreamError },
}

pub(crate) async fn probe_atomic_batch(
    context: &jetstream::Context,
    bucket: &BucketName,
    options: &ProvisionOptions,
) -> Result<(), ProbeError> {
    let id = BatchId::generate()?;
    let stream = bucket.probe_stream_name();
    let subject_root = bucket.probe_subject_root();
    match context.delete_stream(&stream).await {
        Ok(_) => {}
        Err(source) if is_stream_missing(&source) => {}
        Err(source) => return Err(ProbeError::Leftover { stream, source }),
    }
    context
        .create_stream(stream::Config {
            name: stream.clone(),
            subjects: vec![format!("{subject_root}.>")],
            max_messages_per_subject: 1,
            storage: options.storage,
            num_replicas: usize::from(options.replicas.get()),
            allow_atomic_publish: true,
            ..Default::default()
        })
        .await
        .map_err(ProbeError::Create)?;
    let outcome = run_probe(context, &format!("{subject_root}.{id}")).await;
    let cleanup = context
        .delete_stream(&stream)
        .await
        .map(drop)
        .map_err(|source| ProbeError::Cleanup { stream, source });
    outcome.and(cleanup)
}

fn is_stream_missing(error: &DeleteStreamError) -> bool {
    matches!(
        error.kind(),
        DeleteStreamErrorKind::JetStream(ref err) if err.error_code() == ErrorCode::STREAM_NOT_FOUND
    )
}

async fn run_probe(context: &jetstream::Context, subject_root: &str) -> Result<(), ProbeError> {
    let mut batch = AtomicBatch::new()?;
    for index in 0..BATCH_MAX_MESSAGES {
        let subject = Subject::from(format!("{subject_root}.{index}"));
        batch.push(BatchRecord::put(subject, Bytes::new(), Expected::Empty))?;
    }
    match batch.publish(&context.client()).await? {
        BatchOutcome::Committed(ack) if usize::from(ack.last().get()) == BATCH_MAX_MESSAGES => Ok(()),
        BatchOutcome::Committed(ack) => Err(ProbeError::Partial {
            expected: BATCH_MAX_MESSAGES,
            found: ack.last().get(),
        }),
        BatchOutcome::Rejected(rejection) => Err(ProbeError::Refused {
            code: rejection.code().0,
            description: rejection.description().to_owned(),
        }),
        BatchOutcome::Unknown => Err(ProbeError::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::RANDOM_ID_BYTES;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn metadata_round_trips_through_a_map() -> TestResult {
        let config = PresenceConfig::default().with_writer_mode(WriterMode::Managed);
        let metadata = BucketMetadata::new(StreamGeneration::from([5u8; RANDOM_ID_BYTES]), &config);
        let mut map = metadata.to_map();
        map.insert("_nats.level".to_owned(), "3".to_owned());
        assert_eq!(BucketMetadata::from_map(&map), Ok(metadata));
        assert_eq!(metadata.check_against(&config), Ok(()));
        assert_eq!(
            metadata.check_against(&PresenceConfig::default()),
            Err(BucketField::WriterMode)
        );
        Ok(())
    }

    #[test]
    fn metadata_names_the_missing_or_inconsistent_field() {
        let config = PresenceConfig::default();
        let metadata = BucketMetadata::new(StreamGeneration::from([5u8; RANDOM_ID_BYTES]), &config);
        for (key, field) in [
            (SCHEMA_METADATA_KEY, BucketField::Schema),
            (GENERATION_METADATA_KEY, BucketField::Generation),
            (SHARD_COUNT_METADATA_KEY, BucketField::ShardCount),
            (TOKEN_WIDTH_METADATA_KEY, BucketField::TokenWidth),
            (WRITER_MODE_METADATA_KEY, BucketField::WriterMode),
        ] {
            let mut map = metadata.to_map();
            map.remove(key);
            assert_eq!(BucketMetadata::from_map(&map), Err(field), "{key}");
        }
        let mut wrong_width = metadata.to_map();
        wrong_width.insert(TOKEN_WIDTH_METADATA_KEY.to_owned(), "3".to_owned());
        assert_eq!(BucketMetadata::from_map(&wrong_width), Err(BucketField::TokenWidth));
    }

    #[test]
    fn fingerprint_changes_with_creation_time_and_round_trips() -> TestResult {
        let at = |nanos| StreamIdentity {
            name: "KV_PRESENCE_V1".to_owned(),
            created_nanos: nanos,
        };
        let first = StreamFingerprint::of(&[at(1)]);
        assert_eq!(first, StreamFingerprint::of(&[at(1)]));
        assert_ne!(first, StreamFingerprint::of(&[at(2)]));
        assert_eq!(first.to_string().parse::<StreamFingerprint>(), Ok(first));
        Ok(())
    }
}
