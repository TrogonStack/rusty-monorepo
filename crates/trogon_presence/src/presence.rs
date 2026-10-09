use std::sync::{Arc, Mutex, PoisonError};

use async_nats::jetstream::context::{CreateStreamError, CreateStreamErrorKind, GetStreamError, GetStreamErrorKind};
use async_nats::jetstream::stream;
use async_nats::jetstream::{self, ErrorCode};
use tokio::sync::broadcast;

use crate::batch::InflightBatches;
use crate::bucket::{
    check_placement, check_settings, data_stream_config, probe_atomic_batch, BucketField, BucketMetadata, BucketReport,
    ProbeError, StreamFingerprint, StreamIdentity, WriterMode,
};
use crate::config::{BucketName, PresenceConfig, ProvisionOptions};
use crate::constants::FINGERPRINT_METADATA_KEY;
use crate::entropy::EntropyError;
use crate::heartbeat::{Heartbeat, PresenceEvent};
use crate::holder::HolderId;
use crate::inventory::{EntryScope, Inventory};
use crate::key::PresenceKey;
use crate::kv_key::KvKeyError;
use crate::managed::{KeyCoordinator, ManagedLimits, OwnerDeadline};
use crate::position::{OwnerId, StreamGeneration};
use crate::revision::EntryRevision;
use crate::store::KvWriter;
use crate::topic::Topic;
use crate::tracker::{HolderBusy, Tracker, UntrackAllError, UntrackReport};
use crate::watch::replay::{RebuildBudget, ReconnectAudit, ReplayConsumer, ReplayError};
use crate::watch::{CoalesceWindow, MetaFetcher, NoopFetcher, TopicWatch, WatchError, WatchOptions};

const SERVER_METADATA_PREFIX: &str = "_nats.";

#[derive(Clone)]
pub struct Presence {
    config: PresenceConfig,
    client: async_nats::Client,
    stream: stream::Stream,
    binding: Arc<StreamBinding>,
    heartbeat: Heartbeat,
    inflight: InflightBatches,
    _scheduler: Arc<SchedulerGuard>,
}

struct SchedulerGuard(Heartbeat);

impl Drop for SchedulerGuard {
    fn drop(&mut self) {
        self.0.stop();
    }
}

struct StreamBinding {
    generation: StreamGeneration,
    writer_mode: WriterMode,
    fingerprint: StreamFingerprint,
    last_sequence: Mutex<EntryRevision>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("bucket {bucket} has an incompatible {field}")]
pub struct Incompatible {
    pub bucket: BucketName,
    pub field: BucketField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadyReason {
    StreamMissing,
    FingerprintChanged,
    GenerationChanged,
    SequenceRolledBack,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("bucket {0} does not exist; provision it first")]
    BucketMissing(BucketName),
    #[error("could not read bucket {bucket}: {source}")]
    Lookup { bucket: BucketName, source: GetStreamError },
    #[error(transparent)]
    Incompatible(#[from] Incompatible),
    #[error("bucket {bucket} is not ready: {reason:?}")]
    Unready { bucket: BucketName, reason: UnreadyReason },
}

#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error(transparent)]
    HolderBusy(#[from] HolderBusy),
    #[error("bucket {bucket} admits {found} writers, a tracker needs {expected}")]
    WriterMode {
        bucket: BucketName,
        expected: WriterMode,
        found: WriterMode,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error("the server cannot commit an atomic batch: {0}")]
    Probe(#[from] ProbeError),
    #[error("could not mint a stream generation: {0}")]
    Entropy(#[from] EntropyError),
    #[error("could not read bucket {bucket}: {source}")]
    Lookup { bucket: BucketName, source: GetStreamError },
    #[error("could not create bucket {bucket}: {source}")]
    Create {
        bucket: BucketName,
        source: CreateStreamError,
    },
    #[error("could not record the fingerprint of bucket {bucket}: {source}")]
    Fingerprint {
        bucket: BucketName,
        source: CreateStreamError,
    },
    #[error(transparent)]
    Incompatible(#[from] Incompatible),
    #[error(transparent)]
    Open(#[from] OpenError),
}

impl Presence {
    pub async fn provision(
        client: async_nats::Client,
        config: PresenceConfig,
        options: ProvisionOptions,
    ) -> Result<Self, ProvisionError> {
        let context = config.context(client.clone());
        let bucket = config.bucket().clone();
        probe_atomic_batch(&context, &bucket, &options).await?;
        match context.get_stream(bucket.stream_name()).await {
            Ok(stream) => verify_provisioned(stream.cached_info(), &config, &options)?,
            Err(source) if is_stream_missing(&source) => create(&context, &config, &options).await?,
            Err(source) => return Err(ProvisionError::Lookup { bucket, source }),
        }
        Ok(Self::open(client, config).await?)
    }

    pub async fn open(client: async_nats::Client, config: PresenceConfig) -> Result<Self, OpenError> {
        let context = config.context(client);
        let bucket = config.bucket().clone();
        let stream = match context.get_stream(bucket.stream_name()).await {
            Ok(stream) => stream,
            Err(source) if is_stream_missing(&source) => return Err(OpenError::BucketMissing(bucket)),
            Err(source) => return Err(OpenError::Lookup { bucket, source }),
        };
        let info = stream.cached_info();
        let metadata = verify_compatible(&info.config, &config)?;
        let fingerprint = StreamFingerprint::of(&[StreamIdentity::of(info)]);
        if StreamFingerprint::recorded(&info.config.metadata) != Some(fingerprint) {
            return Err(OpenError::Unready {
                bucket,
                reason: UnreadyReason::FingerprintChanged,
            });
        }
        let binding = Arc::new(StreamBinding {
            generation: metadata.generation(),
            writer_mode: metadata.writer_mode(),
            fingerprint,
            last_sequence: Mutex::new(EntryRevision::from(info.state.last_sequence)),
        });
        let client = context.client();
        let watch_stream = stream.clone();
        let writer = KvWriter::new(
            client.clone(),
            stream,
            config.bucket().clone(),
            config.lease_ttl(),
            config.marker_ttl(),
            config.route().clone(),
        );
        let heartbeat = Heartbeat::new(writer, config.heartbeat());
        Ok(Self {
            inflight: InflightBatches::new(config.inflight_batches()),
            config,
            client,
            stream: watch_stream,
            binding,
            _scheduler: Arc::new(SchedulerGuard(heartbeat.clone())),
            heartbeat,
        })
    }

    pub async fn inspect_bucket(
        client: async_nats::Client,
        config: &PresenceConfig,
        placement: Option<&ProvisionOptions>,
    ) -> Result<BucketReport, OpenError> {
        let context = config.context(client);
        let bucket = config.bucket().clone();
        match context.get_stream(bucket.stream_name()).await {
            Ok(stream) => Ok(BucketReport::of_data_stream(stream.cached_info(), config, placement)),
            Err(source) if is_stream_missing(&source) => Err(OpenError::BucketMissing(bucket)),
            Err(source) => Err(OpenError::Lookup { bucket, source }),
        }
    }

    pub async fn entries(&self, scope: EntryScope) -> Result<Inventory, ReplayError> {
        let mut inventory = Inventory::default();
        let filters = scope.filters(self.config.bucket(), self.config.shards());
        let replay = ReplayConsumer::rebuild(&self.stream, &filters, RebuildBudget::default(), |record| {
            inventory.absorb(&scope, &self.config, &record);
        })
        .await?;
        replay.discard();
        inventory.sort();
        Ok(inventory)
    }

    pub async fn verify_ready(&self) -> Result<(), OpenError> {
        let bucket = self.config.bucket().clone();
        let unready = |reason| OpenError::Unready {
            bucket: bucket.clone(),
            reason,
        };
        let context = self.config.context(self.client.clone());
        let stream = match context.get_stream(bucket.stream_name()).await {
            Ok(stream) => stream,
            Err(source) if is_stream_missing(&source) => return Err(unready(UnreadyReason::StreamMissing)),
            Err(source) => return Err(OpenError::Lookup { bucket, source }),
        };
        let info = stream.cached_info();
        if StreamFingerprint::of(&[StreamIdentity::of(info)]) != self.binding.fingerprint {
            return Err(unready(UnreadyReason::FingerprintChanged));
        }
        let metadata = verify_compatible(&info.config, &self.config)?;
        if metadata.generation() != self.binding.generation {
            return Err(unready(UnreadyReason::GenerationChanged));
        }
        let observed = EntryRevision::from(info.state.last_sequence);
        let mut recorded = self
            .binding
            .last_sequence
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if observed < *recorded {
            return Err(unready(UnreadyReason::SequenceRolledBack));
        }
        *recorded = observed;
        Ok(())
    }

    pub fn generation(&self) -> StreamGeneration {
        self.binding.generation
    }

    pub fn fingerprint(&self) -> StreamFingerprint {
        self.binding.fingerprint
    }

    pub fn config(&self) -> &PresenceConfig {
        &self.config
    }

    pub fn tracker(&self) -> Result<Tracker, TrackerError> {
        self.tracker_for(HolderId::generate()?)
    }

    pub fn tracker_for(&self, holder: HolderId) -> Result<Tracker, TrackerError> {
        let found = self.binding.writer_mode;
        if found != WriterMode::Direct {
            return Err(TrackerError::WriterMode {
                bucket: self.config.bucket().clone(),
                expected: WriterMode::Direct,
                found,
            });
        }
        Ok(self.heartbeat.tracker_for(holder, self.config.shards())?)
    }

    pub fn writer_mode(&self) -> WriterMode {
        self.binding.writer_mode
    }

    pub fn coordinator(
        &self,
        key: PresenceKey,
        owner: OwnerId,
        deadline: OwnerDeadline,
        limits: ManagedLimits,
    ) -> Result<KeyCoordinator, KvKeyError> {
        KeyCoordinator::new(
            self.heartbeat.writer().clone(),
            self.config.shards(),
            key,
            owner,
            deadline,
            limits,
            self.inflight.clone(),
        )
    }

    pub async fn watch(&self, topic: Topic) -> Result<TopicWatch, WatchError> {
        self.watch_with(topic, CoalesceWindow::default(), NoopFetcher).await
    }

    pub async fn watch_with<F: MetaFetcher>(
        &self,
        topic: Topic,
        window: CoalesceWindow,
        fetcher: F,
    ) -> Result<TopicWatch, WatchError> {
        self.watch_with_options(topic, WatchOptions::default().with_window(window), fetcher)
            .await
    }

    pub async fn watch_with_options<F: MetaFetcher>(
        &self,
        topic: Topic,
        options: WatchOptions,
        fetcher: F,
    ) -> Result<TopicWatch, WatchError> {
        TopicWatch::start(
            self.stream.clone(),
            ReconnectAudit::new(self.client.statistics()),
            &self.config,
            self.generation(),
            topic,
            options,
            fetcher,
        )
        .await
    }

    pub fn events(&self) -> broadcast::Receiver<PresenceEvent> {
        self.heartbeat.subscribe()
    }

    pub fn close(&self) {
        self.heartbeat.stop();
    }

    pub async fn close_and_untrack(&self) -> Result<(), UntrackAllError> {
        self.close_and_report().await.into_result()
    }

    pub async fn close_and_report(&self) -> UntrackReport {
        self.close();
        let mut report = UntrackReport::default();
        for tracker in self.heartbeat.trackers() {
            if let Ok(partial) = tracker.untrack_all().await {
                report.extend(partial);
            }
        }
        report
    }
}

fn is_stream_missing(error: &GetStreamError) -> bool {
    matches!(
        error.kind(),
        GetStreamErrorKind::JetStream(ref err) if err.error_code() == ErrorCode::STREAM_NOT_FOUND
    )
}

fn verify_compatible(found: &stream::Config, config: &PresenceConfig) -> Result<BucketMetadata, Incompatible> {
    let incompatible = |field| Incompatible {
        bucket: config.bucket().clone(),
        field,
    };
    check_settings(found, config).map_err(incompatible)?;
    let metadata = BucketMetadata::from_map(&found.metadata).map_err(incompatible)?;
    metadata.check_against(config).map_err(incompatible)?;
    if StreamFingerprint::recorded(&found.metadata).is_none() {
        return Err(incompatible(BucketField::Fingerprint));
    }
    Ok(metadata)
}

fn verify_provisioned(
    info: &stream::Info,
    config: &PresenceConfig,
    options: &ProvisionOptions,
) -> Result<(), Incompatible> {
    check_settings(&info.config, config)
        .and_then(|()| check_placement(&info.config, options))
        .map_err(|field| Incompatible {
            bucket: config.bucket().clone(),
            field,
        })?;
    verify_compatible(&info.config, config).map(drop)
}

async fn create(
    context: &jetstream::Context,
    config: &PresenceConfig,
    options: &ProvisionOptions,
) -> Result<(), ProvisionError> {
    let bucket = config.bucket().clone();
    let metadata = BucketMetadata::new(StreamGeneration::generate()?, config);
    let created = match context
        .create_stream(data_stream_config(config, options, &metadata))
        .await
    {
        Ok(stream) => stream,
        Err(source) if is_name_taken(&source) => {
            return match context.get_stream(bucket.stream_name()).await {
                Ok(stream) => Ok(verify_provisioned(stream.cached_info(), config, options)?),
                Err(source) => Err(ProvisionError::Lookup { bucket, source }),
            };
        }
        Err(source) => return Err(ProvisionError::Create { bucket, source }),
    };
    let info = created.cached_info();
    let fingerprint = StreamFingerprint::of(&[StreamIdentity::of(info)]);
    let mut stamped = info.config.clone();
    stamped
        .metadata
        .retain(|key, _| !key.starts_with(SERVER_METADATA_PREFIX));
    stamped
        .metadata
        .insert(FINGERPRINT_METADATA_KEY.to_owned(), fingerprint.to_string());
    context
        .update_stream(&stamped)
        .await
        .map(drop)
        .map_err(|source| ProvisionError::Fingerprint { bucket, source })
}

fn is_name_taken(error: &CreateStreamError) -> bool {
    matches!(
        error.kind(),
        CreateStreamErrorKind::JetStream(ref err) if err.error_code() == ErrorCode::STREAM_NAME_EXIST
    )
}
