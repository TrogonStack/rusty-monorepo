use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_nats::header::NATS_MARKER_REASON;
use async_nats::jetstream::stream::{self, DiscardPolicy, LastRawMessageError, LastRawMessageErrorKind, StorageType};
use async_nats::jetstream::{self};
use async_nats::HeaderMap;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::Serialize;
use trogon_presence::{
    AtomicBatch, BatchError, BatchOutcome, BatchPublishError, BatchRecord, BucketField, EntropyError, EntryRevision,
    Expected, MessageTtl, StreamSetupError, StreamSpec, WholeSecondsError,
};

use crate::rows::{AuthKey, AUTH_ROW_SCHEMA_V1};

pub const DEFAULT_AUTH_BUCKET: &str = "PRESENCE_AUTH_V1";
pub const DEFAULT_MARKER_TTL: Duration = Duration::from_secs(60);
const MAX_VALUE_BYTES: i32 = 1024 * 1024;
const KV_OPERATION_HEADER: &str = "KV-Operation";
const KV_OPERATION_PURGE: &str = "PURGE";
const KV_OPERATION_DELETE: &str = "DEL";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthBucket(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bucket name must be 1 to 64 bytes of [A-Za-z0-9_-]")]
pub struct AuthBucketError;

impl AuthBucket {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn stream_name(&self) -> String {
        format!("KV_{}", self.0)
    }

    pub fn subject(&self, key: &AuthKey) -> async_nats::Subject {
        async_nats::Subject::from(format!("$KV.{}.{key}", self.0))
    }

    pub fn subjects_filter(&self) -> String {
        format!("$KV.{}.>", self.0)
    }

    pub fn segment_filter(&self, segment: &str) -> String {
        format!("$KV.{}.{segment}.>", self.0)
    }

    pub fn key_of<'a>(&self, subject: &'a str) -> Option<&'a str> {
        subject
            .strip_prefix("$KV.")
            .and_then(|rest| rest.strip_prefix(self.0.as_str()))
            .and_then(|rest| rest.strip_prefix('.'))
    }
}

impl Default for AuthBucket {
    fn default() -> Self {
        Self(DEFAULT_AUTH_BUCKET.to_owned())
    }
}

impl FromStr for AuthBucket {
    type Err = AuthBucketError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid =
            !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if !valid {
            return Err(AuthBucketError);
        }
        Ok(Self(s.to_owned()))
    }
}

impl fmt::Display for AuthBucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct AuthProvisionOptions {
    pub replicas: usize,
    pub storage: StorageType,
}

impl Default for AuthProvisionOptions {
    fn default() -> Self {
        Self {
            replicas: 1,
            storage: StorageType::File,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Setup(#[from] StreamSetupError),
    #[error("reading {key}: {source}")]
    Read { key: AuthKey, source: LastRawMessageError },
    #[error("value at {key} is not valid: {reason}")]
    Corrupt { key: AuthKey, reason: String },
    #[error("encoding value: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("building batch: {0}")]
    Batch(#[from] BatchError),
    #[error("publishing batch: {0}")]
    Publish(#[from] BatchPublishError),
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error(transparent)]
    Ttl(#[from] WholeSecondsError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row<T> {
    Missing,
    Removed(EntryRevision),
    Present(EntryRevision, T),
}

impl<T> Row<T> {
    pub fn expected(&self) -> Expected {
        match self {
            Self::Missing => Expected::Empty,
            Self::Removed(revision) | Self::Present(revision, _) => Expected::At(*revision),
        }
    }

    pub fn present(self) -> Option<(EntryRevision, T)> {
        match self {
            Self::Present(revision, value) => Some((revision, value)),
            Self::Missing | Self::Removed(_) => None,
        }
    }
}

#[derive(Debug)]
pub struct Write {
    key: AuthKey,
    payload: Bytes,
    expected: Expected,
    ttl: Option<MessageTtl>,
}

impl Write {
    pub fn put(key: AuthKey, value: &impl Serialize, expected: Expected) -> Result<Self, StoreError> {
        Ok(Self {
            key,
            payload: Bytes::from(serde_json::to_vec(value)?),
            expected,
            ttl: None,
        })
    }

    pub fn expiring(self, ttl: Duration) -> Result<Self, StoreError> {
        let whole = Duration::from_secs(ttl.as_secs().max(1));
        Ok(Self {
            ttl: Some(MessageTtl::try_from(whole)?),
            ..self
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    Committed,
    Conflict,
    Rejected(String),
    Unknown,
}

pub fn auth_stream_spec(bucket: &AuthBucket, options: &AuthProvisionOptions) -> StreamSpec {
    StreamSpec::new(
        stream::Config {
            name: bucket.stream_name(),
            subjects: vec![bucket.subjects_filter()],
            max_messages_per_subject: 1,
            max_message_size: MAX_VALUE_BYTES,
            storage: options.storage,
            num_replicas: options.replicas,
            allow_rollup: true,
            deny_delete: true,
            allow_direct: false,
            discard: DiscardPolicy::New,
            allow_message_ttl: true,
            allow_atomic_publish: true,
            subject_delete_marker_ttl: Some(DEFAULT_MARKER_TTL),
            ..Default::default()
        },
        check_auth_stream,
    )
}

fn check_auth_stream(wanted: &stream::Config, found: &stream::Config) -> Result<(), BucketField> {
    let checks = [
        (found.subjects == wanted.subjects, BucketField::Subjects),
        (found.max_messages_per_subject == 1, BucketField::History),
        (found.discard == DiscardPolicy::New, BucketField::Discard),
        (found.allow_message_ttl, BucketField::MessageTtl),
        (
            found.subject_delete_marker_ttl == wanted.subject_delete_marker_ttl,
            BucketField::MarkerTtl,
        ),
        (found.allow_rollup, BucketField::Rollup),
        (found.deny_delete, BucketField::DenyDelete),
        (found.allow_atomic_publish, BucketField::AtomicPublish),
        (!found.allow_direct, BucketField::DirectGet),
    ];
    checks
        .into_iter()
        .find_map(|(ok, field)| (!ok).then_some(field))
        .map_or(Ok(()), Err)
}

#[derive(Debug, Clone)]
pub struct AuthStore {
    client: async_nats::Client,
    stream: stream::Stream,
    bucket: AuthBucket,
}

impl AuthStore {
    pub async fn provision(
        client: async_nats::Client,
        bucket: AuthBucket,
        options: &AuthProvisionOptions,
    ) -> Result<Self, StoreError> {
        let context = jetstream::new(client.clone());
        let stream = auth_stream_spec(&bucket, options).create_or_verify(&context).await?;
        Ok(Self { client, stream, bucket })
    }

    pub async fn open(client: async_nats::Client, bucket: AuthBucket) -> Result<Self, StoreError> {
        let context = jetstream::new(client.clone());
        let stream = auth_stream_spec(&bucket, &AuthProvisionOptions::default())
            .open(&context)
            .await?;
        Ok(Self { client, stream, bucket })
    }

    pub fn bucket(&self) -> &AuthBucket {
        &self.bucket
    }

    pub async fn read<T: DeserializeOwned>(&self, key: &AuthKey) -> Result<Row<T>, StoreError> {
        let message = match self
            .stream
            .get_last_raw_message_by_subject(&self.bucket.subject(key))
            .await
        {
            Ok(message) => message,
            Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => return Ok(Row::Missing),
            Err(source) => {
                return Err(StoreError::Read {
                    key: key.clone(),
                    source,
                })
            }
        };
        let revision = EntryRevision::from(message.sequence);
        Ok(match decode_row(key, Some(&message.headers), &message.payload)? {
            Some(row) => Row::Present(revision, row),
            None => Row::Removed(revision),
        })
    }

    pub fn stream(&self) -> &stream::Stream {
        &self.stream
    }

    pub async fn commit(&self, writes: Vec<Write>) -> Result<WriteOutcome, StoreError> {
        let mut batch = AtomicBatch::new()?;
        for write in writes {
            let record = BatchRecord::put(self.bucket.subject(&write.key), write.payload, write.expected);
            batch.push(match write.ttl {
                Some(ttl) => record.with_ttl(ttl),
                None => record,
            })?;
        }
        Ok(match batch.publish(&self.client).await? {
            BatchOutcome::Committed(_) => WriteOutcome::Committed,
            BatchOutcome::Rejected(rejection) if rejection.is_wrong_last_sequence() => WriteOutcome::Conflict,
            BatchOutcome::Rejected(rejection) => WriteOutcome::Rejected(rejection.description().to_owned()),
            BatchOutcome::Unknown => WriteOutcome::Unknown,
        })
    }
}

/// Decodes a stored auth row, returning `None` for delete markers and KV tombstones.
pub fn decode_row<T: DeserializeOwned>(
    key: &AuthKey,
    headers: Option<&HeaderMap>,
    payload: &[u8],
) -> Result<Option<T>, StoreError> {
    let is_marker = headers.is_some_and(|headers| headers.get(NATS_MARKER_REASON).is_some());
    let is_tombstone = headers
        .and_then(|headers| headers.get(KV_OPERATION_HEADER))
        .is_some_and(|op| op.as_str() == KV_OPERATION_PURGE || op.as_str() == KV_OPERATION_DELETE);
    if is_marker || is_tombstone {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|err| StoreError::Corrupt {
        key: key.clone(),
        reason: err.to_string(),
    })?;
    let schema = value.get("v").and_then(serde_json::Value::as_u64);
    if schema != Some(u64::from(AUTH_ROW_SCHEMA_V1)) {
        return Err(StoreError::Corrupt {
            key: key.clone(),
            reason: format!("unsupported schema version {schema:?}"),
        });
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|err| StoreError::Corrupt {
            key: key.clone(),
            reason: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_settings_follow_the_contract() {
        let spec = auth_stream_spec(&AuthBucket::default(), &AuthProvisionOptions::default());
        let config = spec.config();
        assert_eq!(config.name, "KV_PRESENCE_AUTH_V1");
        assert!(!config.allow_direct);
        assert!(config.allow_atomic_publish && config.allow_message_ttl && config.allow_rollup && config.deny_delete);
        assert_eq!(config.max_messages_per_subject, 1);
        assert_eq!(config.discard, DiscardPolicy::New);
        assert_eq!(config.subject_delete_marker_ttl, Some(DEFAULT_MARKER_TTL));
        assert_eq!(check_auth_stream(config, config), Ok(()));
        let direct = stream::Config {
            allow_direct: true,
            ..config.clone()
        };
        assert_eq!(check_auth_stream(config, &direct), Err(BucketField::DirectGet));
    }
}
