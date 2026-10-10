mod report;

use std::collections::BTreeMap;
use std::time::Duration;

use async_nats::{Request, RequestError};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use trogon_presence::watch::replay::ReplayError;
use trogon_presence::{
    BucketField, BucketName, BucketReport, EntropyError, EntryScope, HolderId, LifetimeId, MutationSequence, OpenError,
    OwnerEpoch, OwnerId, Presence, PresenceKey, ProvisionOptions, RetryWindow, ShardCount, StoredPresence,
    StreamGeneration, Topic, UnixMillis, ViewShard, WriterShard,
};

use crate::command::{Command, Forwarded, ReleaseWire, RequestIdentity};
use crate::config::ServiceConfig;
use crate::drain::{DrainReply, DrainRequest};
use crate::lease::{inspect_lease_bucket, LeaseError, LeaseHolder, LeaseKey, LeaseStore, LeaseValue};
use crate::reply::{ReplyCode, HEADER_CODE};
use crate::service::{provision, ServiceError};
use crate::subjects::{internal_write_subject, InternalReplyInbox};
use crate::writes::RELEASE_MAX_TARGETS;

pub use self::report::{render, OutputFormat, Report, Table};

const DRAIN_REPLY_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error("could not read the presence bucket: {0}")]
    Replay(#[from] ReplayError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error("could not mint a reply inbox: {0}")]
    Inbox(getrandom::Error),
    #[error("could not encode the request: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("request failed: {0}")]
    Request(#[from] RequestError),
    #[error("refusing to apply: stream {stream} has an incompatible {field}")]
    Refused { stream: String, field: BucketField },
    #[error("lease bucket {0} does not exist; run bucket apply first")]
    LeaseBucketMissing(BucketName),
    #[error("writer shard of key {0} has no owner; is a presence service running?")]
    WriterUnowned(PresenceKey),
    #[error("writer shard of key {0} is owned under another stream generation")]
    GenerationChanged(PresenceKey),
    #[error("the service refused the request with {code}: {detail}")]
    Rejected { code: String, detail: String },
}

pub struct Admin {
    client: async_nats::Client,
    config: ServiceConfig,
    placement: ProvisionOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyAction {
    Created,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppliedBucket {
    pub bucket: BucketName,
    pub action: ApplyAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub data: AppliedBucket,
    pub lease: AppliedBucket,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InspectReport {
    pub entries: Vec<StoredPresence>,
    pub unreadable: usize,
    #[serde(skip)]
    pub now: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CountReport {
    pub topic: Topic,
    pub keys: usize,
    pub metas: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ShardToken(String);

impl ShardToken {
    fn of(shards: ShardCount, shard: ViewShard) -> Self {
        Self(shards.token(shard))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LeaseState {
    Free,
    Held {
        owner: OwnerId,
        revision: trogon_presence::Revision,
        current_generation: bool,
    },
}

impl LeaseState {
    fn of(holder: Option<LeaseHolder>, generation: StreamGeneration) -> Self {
        match holder {
            None => Self::Free,
            Some(holder) => Self::Held {
                owner: holder.value().owner(),
                revision: holder.revision(),
                current_generation: holder.value().generation() == generation,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShardLeases {
    pub shard: ShardToken,
    pub view: LeaseState,
    pub writer: LeaseState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShardsReport {
    pub generation: StreamGeneration,
    pub shards: Vec<ShardLeases>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LeaseBucketCheck {
    Missing { bucket: BucketName },
    Checked(BucketReport),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigReport {
    pub data: BucketReport,
    pub lease: LeaseBucketCheck,
}

impl ConfigReport {
    pub fn has_hard_violation(&self) -> bool {
        self.data.hard_drift().is_some()
            || match &self.lease {
                LeaseBucketCheck::Missing { .. } => true,
                LeaseBucketCheck::Checked(report) => report.hard_drift().is_some(),
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryStatus {
    Released,
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiredEntry {
    pub topic: Topic,
    pub lifetime: LifetimeId,
    pub status: ExpiryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_seq: Option<MutationSequence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpiredKey {
    pub key: PresenceKey,
    pub entries: Vec<ExpiredEntry>,
    pub holder_freed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpireReport {
    pub holder: HolderId,
    pub keys: Vec<ExpiredKey>,
}

#[derive(Debug, Deserialize)]
struct ReleaseReplyWire {
    released: Vec<ExpiredEntry>,
    holder_freed: bool,
}

#[derive(Debug, Deserialize)]
struct ErrorWire {
    error: String,
    #[serde(default)]
    detail: Option<String>,
}

impl Admin {
    pub fn new(client: async_nats::Client, config: ServiceConfig, placement: ProvisionOptions) -> Self {
        Self {
            client,
            config,
            placement,
        }
    }

    pub async fn bucket_apply(&self) -> Result<ApplyReport, AdminError> {
        let data = self.data_report().await?;
        let lease = self.lease_report().await?;
        for report in data.iter().chain(lease.iter()) {
            if let Some(field) = report.any_drift() {
                return Err(AdminError::Refused {
                    stream: report.stream.clone(),
                    field,
                });
            }
        }
        provision(self.client.clone(), &self.config, self.placement.clone()).await?;
        let action = |existing: Option<BucketReport>| match existing {
            Some(_) => ApplyAction::Verified,
            None => ApplyAction::Created,
        };
        Ok(ApplyReport {
            data: AppliedBucket {
                bucket: self.config.presence().bucket().clone(),
                action: action(data),
            },
            lease: AppliedBucket {
                bucket: self.config.lease_bucket().clone(),
                action: action(lease),
            },
        })
    }

    pub async fn config_check(&self) -> Result<ConfigReport, AdminError> {
        let data = Presence::inspect_bucket(self.client.clone(), self.config.presence(), Some(&self.placement)).await?;
        let lease = match self.lease_report().await? {
            Some(report) => LeaseBucketCheck::Checked(report),
            None => LeaseBucketCheck::Missing {
                bucket: self.config.lease_bucket().clone(),
            },
        };
        Ok(ConfigReport { data, lease })
    }

    pub async fn inspect(&self, topic: Topic, key: Option<PresenceKey>) -> Result<InspectReport, AdminError> {
        let scope = match key {
            Some(key) => EntryScope::KeyInTopic(key, topic),
            None => EntryScope::Topic(topic),
        };
        let inventory = self.read(scope).await?;
        Ok(InspectReport {
            unreadable: inventory.unreadable(),
            entries: inventory.into_entries(),
            now: UnixMillis::now(),
        })
    }

    pub async fn count(&self, topic: Topic) -> Result<CountReport, AdminError> {
        let inventory = self.read(EntryScope::Topic(topic.clone())).await?;
        let count = inventory.count_at(UnixMillis::now());
        Ok(CountReport {
            topic,
            keys: count.keys,
            metas: count.metas,
        })
    }

    pub async fn shards(&self) -> Result<ShardsReport, AdminError> {
        let presence = Presence::open(self.client.clone(), self.config.presence().clone()).await?;
        let generation = presence.generation();
        presence.close();
        let leases = self.leases(generation).await?;
        let shards = self.config.presence().shards();
        let reads = (0..shards.get())
            .filter_map(|index| shards.shard(index).ok())
            .map(|shard| {
                let leases = leases.clone();
                async move {
                    let view = leases.holder(LeaseKey::View(ViewShard::from(shard))).await?;
                    let writer = leases.holder(LeaseKey::Writer(WriterShard::from(shard))).await?;
                    Ok::<_, LeaseError>(ShardLeases {
                        shard: ShardToken::of(shards, ViewShard::from(shard)),
                        view: LeaseState::of(view, generation),
                        writer: LeaseState::of(writer, generation),
                    })
                }
            });
        let shards = join_all(reads).await.into_iter().collect::<Result<Vec<_>, _>>()?;
        Ok(ShardsReport { generation, shards })
    }

    pub async fn expire(&self, holder: HolderId) -> Result<ExpireReport, AdminError> {
        let presence = Presence::open(self.client.clone(), self.config.presence().clone()).await?;
        let generation = presence.generation();
        let inventory = presence.entries(EntryScope::Holder(holder)).await;
        presence.close();
        let mut by_key: BTreeMap<PresenceKey, Vec<ReleaseWire>> = BTreeMap::new();
        for entry in inventory?.into_entries() {
            by_key
                .entry(entry.key)
                .or_default()
                .push(ReleaseWire::new(entry.topic, entry.lifetime));
        }
        let leases = self.leases(generation).await?;
        let mut keys = Vec::with_capacity(by_key.len());
        for (key, targets) in by_key {
            let mut expired = ExpiredKey {
                key: key.clone(),
                entries: Vec::with_capacity(targets.len()),
                holder_freed: false,
            };
            for chunk in targets.chunks(RELEASE_MAX_TARGETS) {
                let reply = self.release(&leases, generation, &key, holder, chunk.to_vec()).await?;
                expired.entries.extend(reply.released);
                expired.holder_freed = reply.holder_freed;
            }
            keys.push(expired);
        }
        Ok(ExpireReport { holder, keys })
    }

    pub async fn drain(&self, instance: OwnerId) -> Result<DrainReply, AdminError> {
        let request = DrainRequest::new(instance);
        let payload = serde_json::to_vec(&request)?;
        let message = self
            .client
            .send_request(
                request.subject(),
                Request::new()
                    .payload(payload.into())
                    .inbox(InternalReplyInbox::generate().map_err(AdminError::Inbox)?.into_string())
                    .timeout(Some(DRAIN_REPLY_DEADLINE)),
            )
            .await?;
        decode_reply(&message)
    }

    async fn release(
        &self,
        leases: &LeaseStore,
        generation: StreamGeneration,
        key: &PresenceKey,
        holder: HolderId,
        targets: Vec<ReleaseWire>,
    ) -> Result<ReleaseReplyWire, AdminError> {
        let shards = self.config.presence().shards();
        let owner = leases
            .holder(LeaseKey::Writer(WriterShard::of(key, shards)))
            .await?
            .ok_or_else(|| AdminError::WriterUnowned(key.clone()))?;
        if owner.value().generation() != generation {
            return Err(AdminError::GenerationChanged(key.clone()));
        }
        let request = RequestIdentity::mint(RetryWindow::for_ttl(self.config.presence().lease_ttl()))?;
        let envelope = Forwarded::new(
            key.clone(),
            OwnerEpoch::new(owner.revision(), owner.value().owner()),
            generation,
            None,
            Command::Release {
                request,
                holder,
                targets,
            },
        );
        let message = self
            .client
            .send_request(
                internal_write_subject(shards, key, None),
                Request::new()
                    .payload(serde_json::to_vec(&envelope)?.into())
                    .inbox(InternalReplyInbox::generate().map_err(AdminError::Inbox)?.into_string())
                    .timeout(Some(self.config.writer_reply_deadline().get())),
            )
            .await?;
        decode_reply(&message)
    }

    async fn read(&self, scope: EntryScope) -> Result<trogon_presence::Inventory, AdminError> {
        let presence = Presence::open(self.client.clone(), self.config.presence().clone()).await?;
        let inventory = presence.entries(scope).await;
        presence.close();
        Ok(inventory?)
    }

    async fn leases(&self, generation: StreamGeneration) -> Result<LeaseStore, AdminError> {
        let bucket = self.config.lease_bucket().clone();
        if self.lease_report().await?.is_none() {
            return Err(AdminError::LeaseBucketMissing(bucket));
        }
        Ok(LeaseStore::open_routed(
            self.client.clone(),
            self.config.presence().route().clone(),
            bucket,
            self.config.presence().shards(),
            self.config.lease_ttl(),
            LeaseValue::new(OwnerId::generate()?, generation),
        )
        .await?)
    }

    async fn data_report(&self) -> Result<Option<BucketReport>, AdminError> {
        match Presence::inspect_bucket(self.client.clone(), self.config.presence(), Some(&self.placement)).await {
            Ok(report) => Ok(Some(report)),
            Err(OpenError::BucketMissing(_)) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    async fn lease_report(&self) -> Result<Option<BucketReport>, AdminError> {
        Ok(inspect_lease_bucket(
            self.client.clone(),
            self.config.presence().route(),
            self.config.lease_bucket(),
            self.config.lease_ttl(),
            Some(&self.placement),
        )
        .await?)
    }
}

fn decode_reply<T: for<'de> Deserialize<'de>>(message: &async_nats::Message) -> Result<T, AdminError> {
    let code = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get(HEADER_CODE))
        .map(|value| value.as_str().to_owned());
    if code.as_deref().is_some_and(|code| code != ReplyCode::Ok.as_str()) {
        let wire: ErrorWire = serde_json::from_slice(&message.payload).unwrap_or(ErrorWire {
            error: code.unwrap_or_default(),
            detail: None,
        });
        return Err(AdminError::Rejected {
            code: wire.error,
            detail: wire.detail.unwrap_or_default(),
        });
    }
    Ok(serde_json::from_slice(&message.payload)?)
}
