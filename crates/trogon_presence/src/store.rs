use async_nats::header::NATS_MARKER_REASON;
use async_nats::jetstream::message::StreamMessage;
use async_nats::jetstream::stream::{LastRawMessageError, LastRawMessageErrorKind, Stream};
use async_nats::Subject;

pub(crate) use crate::batch::Expected;
const SLOW_CALL: std::time::Duration = std::time::Duration::from_secs(1);

use crate::batch::{AtomicBatch, BatchBudget, BatchOutcome, BatchPublishError, Unpaced};
use crate::config::{BucketName, GuardTtl, LeaseTtl, MarkerTtl, ReceiptTtl};
use crate::constants::{KV_OPERATION_DELETE, KV_OPERATION_HEADER, KV_OPERATION_PURGE};
use crate::domain::JetStreamRoute;
use crate::kv_key::KvKey;
use crate::read::ReadRequestTimeout;
use crate::receipt::{GuardBody, Receipt, ReceiptError};
use crate::revision::EntryRevision;
use crate::value::{StoredValue, Tombstone, ValueError};

#[derive(Debug, Clone)]
pub(crate) enum EntryRead {
    Missing,
    Removed {
        revision: EntryRevision,
        tombstone: Option<Tombstone>,
    },
    Live {
        revision: EntryRevision,
        value: Box<StoredValue>,
    },
}

impl EntryRead {
    pub(crate) fn expected(&self) -> Expected {
        match self {
            Self::Missing => Expected::Empty,
            Self::Removed { revision, .. } | Self::Live { revision, .. } => Expected::At(*revision),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ReceiptRead {
    Absent(Expected),
    Found { revision: EntryRevision, receipt: Receipt },
}

#[derive(Debug, Clone)]
pub(crate) enum GuardRead {
    Free(Expected),
    Held { revision: EntryRevision, guard: GuardBody },
}

#[derive(Debug, Clone)]
pub(crate) enum ControlRead {
    Free(Expected),
    Held {
        revision: EntryRevision,
        payload: bytes::Bytes,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("leader read failed: {0}")]
    Read(#[from] LastRawMessageError),
    #[error("stored entry could not be decoded: {0}")]
    Decode(#[from] ValueError),
    #[error("stored receipt could not be decoded: {0}")]
    Receipt(#[from] ReceiptError),
    #[error("stored guard could not be decoded: {0}")]
    Guard(#[from] serde_json::Error),
}

#[derive(Clone)]
pub(crate) struct KvWriter {
    client: async_nats::Client,
    stream: Stream,
    bucket: BucketName,
    lease_ttl: LeaseTtl,
    marker_ttl: MarkerTtl,
    route: JetStreamRoute,
    read_timeout: ReadRequestTimeout,
}

enum Raw {
    Missing,
    Removed(EntryRevision, StreamMessage),
    Present(EntryRevision, StreamMessage),
}

impl KvWriter {
    pub(crate) fn new(
        client: async_nats::Client,
        stream: Stream,
        bucket: BucketName,
        lease_ttl: LeaseTtl,
        marker_ttl: MarkerTtl,
        route: JetStreamRoute,
        read_timeout: ReadRequestTimeout,
    ) -> Self {
        Self {
            client,
            stream,
            bucket,
            lease_ttl,
            marker_ttl,
            route,
            read_timeout,
        }
    }

    pub(crate) fn subject(&self, key: &KvKey) -> Subject {
        Subject::from(self.bucket.subject_for(key))
    }

    pub(crate) fn stream(&self) -> &Stream {
        &self.stream
    }

    pub(crate) fn bucket(&self) -> &BucketName {
        &self.bucket
    }

    pub(crate) fn client(&self) -> &async_nats::Client {
        &self.client
    }

    pub(crate) fn route(&self) -> &JetStreamRoute {
        &self.route
    }

    pub(crate) fn lease_ttl(&self) -> LeaseTtl {
        self.lease_ttl
    }

    pub(crate) fn marker_ttl(&self) -> MarkerTtl {
        self.marker_ttl
    }

    pub(crate) fn guard_ttl(&self) -> GuardTtl {
        GuardTtl::default()
    }

    pub(crate) fn receipt_ttl(&self) -> ReceiptTtl {
        ReceiptTtl::default()
    }

    pub(crate) async fn publish(&self, batch: AtomicBatch) -> Result<BatchOutcome, BatchPublishError> {
        let started = std::time::Instant::now();
        let outcome = batch
            .publish_routed(&self.client, &self.route, BatchBudget::default(), &Unpaced)
            .await;
        if started.elapsed() > SLOW_CALL {
            tracing::warn!(elapsed = ?started.elapsed(), ?outcome, "slow atomic batch publish");
        }
        outcome
    }

    async fn read_raw(&self, key: &KvKey) -> Result<Raw, StoreError> {
        let started = std::time::Instant::now();
        let read = self
            .read_timeout
            .last_message(&self.stream, &self.bucket.subject_for(key))
            .await;
        if started.elapsed() > SLOW_CALL {
            tracing::warn!(elapsed = ?started.elapsed(), error = ?read.as_ref().err(), "slow last-message read");
        }
        match read {
            Ok(message) => {
                let revision = EntryRevision::from(message.sequence);
                let is_marker = message.headers.get(NATS_MARKER_REASON).is_some();
                let is_tombstone = message
                    .headers
                    .get(KV_OPERATION_HEADER)
                    .is_some_and(|op| op.as_str() == KV_OPERATION_PURGE || op.as_str() == KV_OPERATION_DELETE);
                if is_marker || is_tombstone {
                    Ok(Raw::Removed(revision, message))
                } else {
                    Ok(Raw::Present(revision, message))
                }
            }
            Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(Raw::Missing),
            Err(err) => Err(StoreError::Read(err)),
        }
    }

    pub(crate) async fn read_entry(&self, key: &KvKey) -> Result<EntryRead, StoreError> {
        Ok(match self.read_raw(key).await? {
            Raw::Missing => EntryRead::Missing,
            Raw::Removed(revision, message) => EntryRead::Removed {
                revision,
                tombstone: Tombstone::from_json_bytes(&message.payload).ok(),
            },
            Raw::Present(revision, message) => EntryRead::Live {
                revision,
                value: Box::new(StoredValue::from_json_bytes(&message.payload)?),
            },
        })
    }

    pub(crate) async fn read_receipt(&self, key: &KvKey) -> Result<ReceiptRead, StoreError> {
        Ok(match self.read_raw(key).await? {
            Raw::Missing => ReceiptRead::Absent(Expected::Empty),
            Raw::Removed(revision, _) => ReceiptRead::Absent(Expected::At(revision)),
            Raw::Present(revision, message) => ReceiptRead::Found {
                revision,
                receipt: Receipt::from_json_bytes(&message.payload)?,
            },
        })
    }

    pub(crate) async fn read_control(&self, key: &KvKey) -> Result<ControlRead, StoreError> {
        Ok(match self.read_raw(key).await? {
            Raw::Missing => ControlRead::Free(Expected::Empty),
            Raw::Removed(revision, _) => ControlRead::Free(Expected::At(revision)),
            Raw::Present(revision, message) => ControlRead::Held {
                revision,
                payload: message.payload,
            },
        })
    }

    pub(crate) async fn read_guard(&self, key: &KvKey) -> Result<GuardRead, StoreError> {
        Ok(match self.read_raw(key).await? {
            Raw::Missing => GuardRead::Free(Expected::Empty),
            Raw::Removed(revision, _) => GuardRead::Free(Expected::At(revision)),
            Raw::Present(revision, message) => GuardRead::Held {
                revision,
                guard: GuardBody::from_json_bytes(&message.payload)?,
            },
        })
    }
}
