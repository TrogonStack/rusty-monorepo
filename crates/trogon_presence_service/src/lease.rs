use std::time::Duration;

use async_nats::header::{NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, NATS_MARKER_REASON, NATS_MESSAGE_TTL, NATS_ROLLUP};
use async_nats::jetstream::context::{
    CreateStreamErrorKind, GetStreamError, GetStreamErrorKind, PublishError, PublishErrorKind,
};
use async_nats::jetstream::stream::{self, LastRawMessageError, LastRawMessageErrorKind, Stream};
use async_nats::jetstream::{Context, ErrorCode};
use async_nats::{HeaderMap, Subject};
use serde::{Deserialize, Serialize};
use trogon_presence::{
    BucketField, BucketName, BucketReport, DriftSeverity, FieldCheck, JetStreamRoute, OwnerId, ProvisionOptions,
    ReadRequestTimeout, Revision, ShardCount, StreamGeneration, ViewShard, WriterShard,
};

use crate::config::ShardLeaseTtl;

const KV_OPERATION_HEADER: &str = "KV-Operation";
const KV_OPERATION_PURGE: &str = "PURGE";
const KV_OPERATION_DELETE: &str = "DEL";
const ROLLUP_SUBJECT: &str = "sub";
const WRITER_LEASE_PREFIX: &str = "writer";
const VIEW_LEASE_PREFIX: &str = "view";
const ACQUIRE_MAX_ATTEMPTS: usize = 3;
const LEASE_STREAM_MIN_MAX_AGE: Duration = Duration::from_secs(60);
const LEASE_MARKER_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("could not create lease bucket {bucket}: {detail}")]
    Create { bucket: BucketName, detail: String },
    #[error("lease bucket {bucket} has an incompatible {field}")]
    Incompatible { bucket: BucketName, field: BucketField },
    #[error("lease bucket {bucket} is missing or unreadable: {source}")]
    Open { bucket: BucketName, source: GetStreamError },
    #[error("lease publish failed: {0}")]
    Publish(#[from] PublishError),
    #[error("lease leader read failed: {0}")]
    Read(#[from] LastRawMessageError),
    #[error("lease value could not be encoded: {0}")]
    Encode(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseKey {
    Writer(WriterShard),
    View(ViewShard),
}

impl LeaseKey {
    pub fn token(self, shards: ShardCount) -> String {
        match self {
            Self::Writer(shard) => format!("{WRITER_LEASE_PREFIX}.{}", shards.token(shard)),
            Self::View(shard) => format!("{VIEW_LEASE_PREFIX}.{}", shards.token(shard)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseValue {
    owner: OwnerId,
    generation: StreamGeneration,
}

impl LeaseValue {
    pub fn new(owner: OwnerId, generation: StreamGeneration) -> Self {
        Self { owner, generation }
    }

    pub fn owner(self) -> OwnerId {
        self.owner
    }

    pub fn generation(self) -> StreamGeneration {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseHolder {
    revision: Revision,
    value: LeaseValue,
}

impl LeaseHolder {
    pub fn revision(self) -> Revision {
        self.revision
    }

    pub fn value(self) -> LeaseValue {
        self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    Renewed(Revision),
    Lost,
    Unknown,
}

enum Written {
    At(Revision),
    Conflict,
    Unknown,
}

enum Current {
    Free(u64),
    Held {
        revision: Revision,
        value: Option<LeaseValue>,
    },
}

#[derive(Clone)]
pub struct LeaseStore {
    context: Context,
    stream: Stream,
    bucket: BucketName,
    shards: ShardCount,
    ttl: ShardLeaseTtl,
    value: LeaseValue,
    route: JetStreamRoute,
    read_timeout: ReadRequestTimeout,
}

pub async fn provision_lease_bucket(
    client: async_nats::Client,
    route: &JetStreamRoute,
    bucket: &BucketName,
    ttl: ShardLeaseTtl,
    options: ProvisionOptions,
) -> Result<(), LeaseError> {
    let context = route.context(client.clone());
    let config = lease_stream_config(bucket, ttl, &options);
    match context.create_stream(config).await {
        Ok(_) => Ok(()),
        Err(err) => match err.kind() {
            CreateStreamErrorKind::JetStream(ref js) if js.error_code() == ErrorCode::STREAM_NAME_EXIST => {
                match inspect_lease_bucket(client, route, bucket, ttl, Some(&options)).await? {
                    Some(report) => match report.any_drift() {
                        Some(field) => Err(LeaseError::Incompatible {
                            bucket: bucket.clone(),
                            field,
                        }),
                        None => Ok(()),
                    },
                    None => Err(LeaseError::Create {
                        bucket: bucket.clone(),
                        detail: err.to_string(),
                    }),
                }
            }
            _ => Err(LeaseError::Create {
                bucket: bucket.clone(),
                detail: err.to_string(),
            }),
        },
    }
}

fn lease_stream_config(bucket: &BucketName, ttl: ShardLeaseTtl, options: &ProvisionOptions) -> stream::Config {
    stream::Config {
        name: bucket.stream_name(),
        subjects: vec![bucket.subjects_filter()],
        max_messages_per_subject: 1,
        max_age: LEASE_STREAM_MIN_MAX_AGE.max(ttl.get() * 2),
        storage: options.storage,
        num_replicas: usize::from(options.replicas.get()),
        allow_rollup: true,
        deny_delete: true,
        allow_direct: true,
        allow_message_ttl: true,
        subject_delete_marker_ttl: Some(LEASE_MARKER_TTL),
        ..Default::default()
    }
}

pub async fn inspect_lease_bucket(
    client: async_nats::Client,
    route: &JetStreamRoute,
    bucket: &BucketName,
    ttl: ShardLeaseTtl,
    placement: Option<&ProvisionOptions>,
) -> Result<Option<BucketReport>, LeaseError> {
    let context = route.context(client);
    let stream = match context.get_stream(bucket.stream_name()).await {
        Ok(stream) => stream,
        Err(source) if matches!(source.kind(), GetStreamErrorKind::JetStream(ref js) if js.error_code() == ErrorCode::STREAM_NOT_FOUND) => {
            return Ok(None)
        }
        Err(source) => {
            return Err(LeaseError::Open {
                bucket: bucket.clone(),
                source,
            })
        }
    };
    let found = &stream.cached_info().config;
    let expected = lease_stream_config(bucket, ttl, &ProvisionOptions::default());
    let hard = |holds: bool, field: BucketField| FieldCheck::new(field, holds, DriftSeverity::Hard);
    let mut checks = vec![
        hard(found.subjects == expected.subjects, BucketField::Subjects),
        hard(found.retention == expected.retention, BucketField::Retention),
        hard(
            found.max_messages_per_subject == expected.max_messages_per_subject,
            BucketField::History,
        ),
        hard(found.max_age == expected.max_age, BucketField::MaxAge),
        hard(found.allow_rollup, BucketField::Rollup),
        hard(found.deny_delete, BucketField::DenyDelete),
        hard(found.allow_direct, BucketField::DirectGet),
        hard(found.allow_message_ttl, BucketField::MessageTtl),
        hard(
            found.subject_delete_marker_ttl == expected.subject_delete_marker_ttl,
            BucketField::MarkerTtl,
        ),
    ];
    if let Some(options) = placement {
        let advisory = |holds: bool, field: BucketField| FieldCheck::new(field, holds, DriftSeverity::Advisory);
        checks.push(advisory(found.storage == options.storage, BucketField::Storage));
        checks.push(advisory(
            found.num_replicas == usize::from(options.replicas.get()),
            BucketField::Replicas,
        ));
    }
    Ok(Some(BucketReport::new(found.name.clone(), checks)))
}

impl LeaseStore {
    pub async fn open(
        client: async_nats::Client,
        bucket: BucketName,
        shards: ShardCount,
        ttl: ShardLeaseTtl,
        value: LeaseValue,
    ) -> Result<Self, LeaseError> {
        Self::open_routed(client, JetStreamRoute::local(), bucket, shards, ttl, value).await
    }

    pub async fn open_routed(
        client: async_nats::Client,
        route: JetStreamRoute,
        bucket: BucketName,
        shards: ShardCount,
        ttl: ShardLeaseTtl,
        value: LeaseValue,
    ) -> Result<Self, LeaseError> {
        let context = route.context(client);
        let stream = context
            .get_stream(bucket.stream_name())
            .await
            .map_err(|source| LeaseError::Open {
                bucket: bucket.clone(),
                source,
            })?;
        Ok(Self {
            context,
            stream,
            bucket,
            shards,
            ttl,
            value,
            route,
            read_timeout: ReadRequestTimeout::default(),
        })
    }

    pub fn with_read_timeout(self, read_timeout: ReadRequestTimeout) -> Self {
        Self { read_timeout, ..self }
    }

    pub fn ttl(&self) -> ShardLeaseTtl {
        self.ttl
    }

    pub fn value(&self) -> LeaseValue {
        self.value
    }

    pub fn subject(&self, key: LeaseKey) -> String {
        format!("$KV.{}.{}", self.bucket, key.token(self.shards))
    }

    pub async fn acquire(&self, shard: LeaseKey) -> Result<Option<Revision>, LeaseError> {
        let mut expected = 0;
        for _ in 0..ACQUIRE_MAX_ATTEMPTS {
            match self.write(shard, expected).await? {
                Written::At(revision) => return Ok(Some(revision)),
                Written::Conflict => {}
                Written::Unknown => {
                    return Ok(match self.current(shard).await? {
                        Current::Held { revision, value } if value == Some(self.value) => Some(revision),
                        _ => None,
                    });
                }
            }
            match self.current(shard).await? {
                Current::Free(at) => expected = at,
                Current::Held { .. } => return Ok(None),
            }
        }
        Ok(None)
    }

    pub async fn renew(&self, shard: LeaseKey, revision: Revision) -> Result<Renewal, LeaseError> {
        Ok(match self.write(shard, revision.get()).await? {
            Written::At(revision) => Renewal::Renewed(revision),
            Written::Conflict => Renewal::Lost,
            Written::Unknown => Renewal::Unknown,
        })
    }

    pub async fn release(&self, shard: LeaseKey, revision: Revision) -> Result<bool, LeaseError> {
        let mut headers = HeaderMap::new();
        headers.insert(KV_OPERATION_HEADER, KV_OPERATION_PURGE);
        headers.insert(NATS_ROLLUP, ROLLUP_SUBJECT);
        headers.insert(NATS_MESSAGE_TTL, self.ttl.header_value());
        headers.insert(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, revision.to_string());
        Ok(matches!(
            self.publish(shard, headers, Vec::new()).await?,
            Written::At(_)
        ))
    }

    pub async fn holder(&self, shard: LeaseKey) -> Result<Option<LeaseHolder>, LeaseError> {
        Ok(match self.current(shard).await? {
            Current::Held {
                revision,
                value: Some(value),
            } => Some(LeaseHolder { revision, value }),
            Current::Held { value: None, .. } | Current::Free(_) => None,
        })
    }

    async fn write(&self, shard: LeaseKey, expected: u64) -> Result<Written, LeaseError> {
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_TTL, self.ttl.header_value());
        headers.insert(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, expected.to_string());
        let value = serde_json::to_vec(&self.value)?;
        self.publish(shard, headers, value).await
    }

    async fn publish(&self, shard: LeaseKey, headers: HeaderMap, payload: Vec<u8>) -> Result<Written, LeaseError> {
        let ack = match self
            .context
            .publish_with_headers(
                self.route.publish_subject(&Subject::from(self.subject(shard))),
                headers,
                payload.into(),
            )
            .await
        {
            Ok(future) => future.await,
            Err(err) => Err(err),
        };
        match ack {
            Ok(ack) => Ok(Written::At(Revision::from(ack.sequence))),
            Err(err) => match err.kind() {
                PublishErrorKind::WrongLastSequence => Ok(Written::Conflict),
                PublishErrorKind::TimedOut => Ok(Written::Unknown),
                _ => Err(LeaseError::Publish(err)),
            },
        }
    }

    async fn current(&self, shard: LeaseKey) -> Result<Current, LeaseError> {
        match self.read_timeout.last_message(&self.stream, &self.subject(shard)).await {
            Ok(message) => {
                let freed = message.headers.get(NATS_MARKER_REASON).is_some()
                    || message
                        .headers
                        .get(KV_OPERATION_HEADER)
                        .is_some_and(|op| op.as_str() == KV_OPERATION_PURGE || op.as_str() == KV_OPERATION_DELETE);
                Ok(if freed {
                    Current::Free(message.sequence)
                } else {
                    Current::Held {
                        revision: Revision::from(message.sequence),
                        value: serde_json::from_slice::<LeaseValue>(&message.payload).ok(),
                    }
                })
            }
            Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(Current::Free(0)),
            Err(err) => Err(LeaseError::Read(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn lease_keys_name_their_shard_kind() -> TestResult {
        let shards = ShardCount::DEFAULT;
        let shard = shards.shard(7)?;
        assert_eq!(LeaseKey::Writer(WriterShard::from(shard)).token(shards), "writer.s07");
        assert_eq!(LeaseKey::View(ViewShard::from(shard)).token(shards), "view.s07");
        Ok(())
    }

    #[test]
    fn lease_body_carries_owner_and_generation() -> TestResult {
        let value = LeaseValue::new(OwnerId::from([3; 16]), StreamGeneration::from([4; 16]));
        let json: serde_json::Value = serde_json::to_value(value)?;
        assert_eq!(json["owner"], OwnerId::from([3; 16]).to_string());
        assert_eq!(json["generation"], StreamGeneration::from([4; 16]).to_string());
        assert!(json.get("node").is_none());
        Ok(())
    }
}
