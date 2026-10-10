use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::batch::{
    AtomicBatch, BatchError, BatchOutcome, BatchPublishError, BatchRecord, BatchRejection, BatchRevisionError,
};
use crate::constants::{CAS_MAX_ATTEMPTS, HEARTBEAT_ENTRIES_PER_BATCH, TRACKER_COMMAND_BUFFER};
use crate::entropy::EntropyError;
use crate::heartbeat::{jitter, Heartbeat, PresenceEvent};
use crate::holder::HolderId;
use crate::key::PresenceKey;
use crate::kv_key::{AuthorityId, ControlKey, EntryKey, KvKey, KvKeyError};
use crate::meta::Meta;
use crate::operation::{AdmissionError, EntryTarget, IntentBody, RetryWindow, UnixMillis, WriteIntent, WriteRequest};
use crate::phx_ref::StoredRef;
use crate::position::{LifetimeId, MutationSequence, OperationId, OwnerId, PositionOverflow};
use crate::receipt::{GuardBody, Liveness, Receipt, ReceiptError, ReceiptTarget, WriteReceipt};
use crate::revision::EntryRevision;
use crate::shard::ShardCount;
use crate::store::{EntryRead, Expected, GuardRead, KvWriter, ReceiptRead, StoreError};
use crate::topic::Topic;
use crate::value::{StoredValue, Tombstone, ValueError};

#[derive(Debug, Clone, PartialEq)]
pub struct TrackedEntry {
    entry: EntryKey,
    kv_key: KvKey,
    value: StoredValue,
    revision: EntryRevision,
    birth: EntryRevision,
}

impl TrackedEntry {
    pub fn entry(&self) -> &EntryKey {
        &self.entry
    }

    pub fn kv_key(&self) -> &KvKey {
        &self.kv_key
    }

    pub fn value(&self) -> &StoredValue {
        &self.value
    }

    pub fn stored_ref(&self) -> &StoredRef {
        self.value.phx_ref()
    }

    pub fn revision(&self) -> EntryRevision {
        self.revision
    }

    pub fn birth(&self) -> EntryRevision {
        self.birth
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.value.lifetime()
    }

    pub fn sequence(&self) -> MutationSequence {
        self.value.mutation_seq()
    }

    pub fn target(&self) -> EntryTarget {
        EntryTarget::new(self.lifetime(), self.sequence())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    Applied(WriteReceipt),
    Replayed { receipt: WriteReceipt, liveness: Liveness },
    AlreadyLive(WriteReceipt),
    Gone,
}

#[derive(Debug, thiserror::Error)]
#[error("tracker task has stopped")]
pub struct TrackerClosed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("holder {0} is bound to another live or draining tracker")]
pub struct HolderBusy(HolderId);

impl HolderBusy {
    pub(crate) fn new(holder: HolderId) -> Self {
        Self(holder)
    }

    pub fn holder(&self) -> HolderId {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Admission(#[from] AdmissionError),
    #[error("the operation id was already used for a different intent")]
    OperationConflict,
    #[error("the request does not name the current mutation sequence {current}")]
    SequenceConflict { current: MutationSequence },
    #[error("presence was replaced by stored ref {current_ref}")]
    Conflict { current_ref: StoredRef, current_meta: Meta },
    #[error("presence is not tracked by this holder")]
    NotTracked,
    #[error(transparent)]
    HolderBusy(#[from] HolderBusy),
    #[error(transparent)]
    Key(#[from] KvKeyError),
    #[error(transparent)]
    Value(#[from] ValueError),
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error(transparent)]
    Overflow(#[from] PositionOverflow),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Batch(#[from] BatchError),
    #[error(transparent)]
    Publish(#[from] BatchPublishError),
    #[error("the server rejected the batch: {}", .0.description())]
    Rejected(BatchRejection),
    #[error(transparent)]
    Revision(#[from] BatchRevisionError),
    #[error(transparent)]
    Receipt(#[from] ReceiptError),
    #[error("entry kept changing across {0} attempts")]
    Contended(usize),
    #[error(transparent)]
    Closed(#[from] TrackerClosed),
}

#[derive(Debug, thiserror::Error)]
pub enum UntrackAllError {
    #[error("{} entries failed to untrack", .0.len())]
    Failed(Vec<(EntryKey, WriteError)>),
    #[error(transparent)]
    Closed(#[from] TrackerClosed),
}

#[derive(Debug)]
pub struct EntryUntrack {
    entry: EntryKey,
    result: Result<WriteOutcome, WriteError>,
}

impl EntryUntrack {
    pub fn entry(&self) -> &EntryKey {
        &self.entry
    }

    pub fn result(&self) -> &Result<WriteOutcome, WriteError> {
        &self.result
    }
}

#[derive(Debug, Default)]
pub struct UntrackReport {
    outcomes: Vec<EntryUntrack>,
}

impl UntrackReport {
    pub fn outcomes(&self) -> &[EntryUntrack] {
        &self.outcomes
    }

    pub fn is_complete(&self) -> bool {
        self.outcomes.iter().all(|outcome| outcome.result.is_ok())
    }

    pub fn into_result(self) -> Result<(), UntrackAllError> {
        let failures: Vec<(EntryKey, WriteError)> = self
            .outcomes
            .into_iter()
            .filter_map(|outcome| outcome.result.err().map(|err| (outcome.entry, err)))
            .collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(UntrackAllError::Failed(failures))
        }
    }

    pub(crate) fn extend(&mut self, other: UntrackReport) {
        self.outcomes.extend(other.outcomes);
    }
}

#[derive(Debug, Default)]
pub(crate) struct BeatReport {
    pub(crate) failed: bool,
}

type Reply<T> = oneshot::Sender<T>;

enum Command {
    Submit {
        intent: WriteIntent,
        enriched: Option<Meta>,
        reply: Reply<Result<WriteOutcome, WriteError>>,
    },
    Update {
        request: WriteRequest,
        topic: Topic,
        key: PresenceKey,
        meta: Meta,
        expected: Option<StoredRef>,
        reply: Reply<Result<WriteOutcome, WriteError>>,
    },
    Untrack {
        request: WriteRequest,
        topic: Topic,
        key: PresenceKey,
        reply: Reply<Result<WriteOutcome, WriteError>>,
    },
    UntrackAll {
        reply: Reply<UntrackReport>,
    },
    Entries {
        reply: Reply<Vec<TrackedEntry>>,
    },
    Beat {
        timeout: Duration,
        reply: Reply<BeatReport>,
    },
}

#[derive(Clone)]
pub struct Tracker {
    holder: HolderId,
    window: RetryWindow,
    commands: mpsc::Sender<Command>,
}

impl PartialEq for Tracker {
    fn eq(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracker")
            .field("holder", &self.holder)
            .finish_non_exhaustive()
    }
}

pub(crate) struct WeakTracker {
    holder: HolderId,
    window: RetryWindow,
    commands: mpsc::WeakSender<Command>,
}

impl WeakTracker {
    pub(crate) fn upgrade(&self) -> Option<Tracker> {
        self.commands.upgrade().map(|commands| Tracker {
            holder: self.holder,
            window: self.window,
            commands,
        })
    }
}

impl Tracker {
    pub(crate) fn spawn(holder: HolderId, shards: ShardCount, heartbeat: Heartbeat) -> Self {
        let (commands, inbox) = mpsc::channel(TRACKER_COMMAND_BUFFER);
        let writer = heartbeat.writer().clone();
        let window = RetryWindow::for_ttl(writer.lease_ttl());
        let actor = Actor {
            holder,
            shards,
            window,
            writer,
            heartbeat,
            owner: None,
            guard: None,
            entries: HashMap::new(),
        };
        tokio::spawn(actor.run(inbox));
        Self {
            holder,
            window,
            commands,
        }
    }

    pub(crate) fn downgrade(&self) -> WeakTracker {
        WeakTracker {
            holder: self.holder,
            window: self.window,
            commands: self.commands.downgrade(),
        }
    }

    pub fn holder(&self) -> &HolderId {
        &self.holder
    }

    pub fn retry_window(&self) -> RetryWindow {
        self.window
    }

    pub fn request(&self) -> Result<WriteRequest, EntropyError> {
        Ok(WriteRequest::new(
            OperationId::generate()?,
            self.window.open(UnixMillis::now()),
            self.holder,
        ))
    }

    pub async fn submit(&self, intent: WriteIntent, enriched: Option<Meta>) -> Result<WriteOutcome, WriteError> {
        self.call(|reply| Command::Submit {
            intent,
            enriched,
            reply,
        })
        .await?
    }

    pub async fn track(&self, topic: &Topic, key: &PresenceKey, meta: Meta) -> Result<WriteOutcome, WriteError> {
        let intent = self.request()?.track(topic.clone(), key.clone(), meta);
        self.submit(intent, None).await
    }

    pub async fn update(&self, topic: &Topic, key: &PresenceKey, meta: Meta) -> Result<WriteOutcome, WriteError> {
        self.update_command(topic, key, meta, None).await
    }

    pub async fn update_with_expected_ref(
        &self,
        topic: &Topic,
        key: &PresenceKey,
        expected: &StoredRef,
        meta: Meta,
    ) -> Result<WriteOutcome, WriteError> {
        self.update_command(topic, key, meta, Some(expected.clone())).await
    }

    async fn update_command(
        &self,
        topic: &Topic,
        key: &PresenceKey,
        meta: Meta,
        expected: Option<StoredRef>,
    ) -> Result<WriteOutcome, WriteError> {
        let request = self.request()?;
        self.call(|reply| Command::Update {
            request,
            topic: topic.clone(),
            key: key.clone(),
            meta,
            expected,
            reply,
        })
        .await?
    }

    pub async fn untrack(&self, topic: &Topic, key: &PresenceKey) -> Result<WriteOutcome, WriteError> {
        let request = self.request()?;
        self.call(|reply| Command::Untrack {
            request,
            topic: topic.clone(),
            key: key.clone(),
            reply,
        })
        .await?
    }

    pub async fn untrack_all(&self) -> Result<UntrackReport, TrackerClosed> {
        self.call(|reply| Command::UntrackAll { reply }).await
    }

    pub async fn entries(&self) -> Result<Vec<TrackedEntry>, TrackerClosed> {
        self.call(|reply| Command::Entries { reply }).await
    }

    pub(crate) async fn beat(&self, timeout: Duration) -> Result<BeatReport, TrackerClosed> {
        self.call(|reply| Command::Beat { timeout, reply }).await
    }

    async fn call<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, TrackerClosed> {
        let (reply, response) = oneshot::channel();
        self.commands.send(command(reply)).await.map_err(|_| TrackerClosed)?;
        response.await.map_err(|_| TrackerClosed)
    }
}

pub(crate) enum Change {
    Put {
        value: Box<StoredValue>,
        expected: Expected,
    },
    Purge {
        tombstone: Tombstone,
        stored_ref: StoredRef,
        expected: Expected,
    },
}

impl Change {
    pub(crate) fn target(&self, kv_key: &KvKey, position: crate::batch::BatchPosition) -> ReceiptTarget {
        match self {
            Self::Put { value, .. } => ReceiptTarget::new(
                kv_key.clone(),
                position,
                value.lifetime(),
                value.phx_ref().clone(),
                value.mutation_seq(),
            ),
            Self::Purge {
                tombstone, stored_ref, ..
            } => ReceiptTarget::new(
                kv_key.clone(),
                position,
                tombstone.lifetime(),
                stored_ref.clone(),
                tombstone.mutation_seq(),
            ),
        }
    }

    pub(crate) fn record(&self, writer: &KvWriter, kv_key: &KvKey) -> Result<BatchRecord, ValueError> {
        let subject = writer.subject(kv_key);
        Ok(match self {
            Self::Put { value, expected } => {
                BatchRecord::put(subject, value.to_json_bytes()?.into(), *expected).with_ttl(writer.lease_ttl().into())
            }
            Self::Purge {
                tombstone, expected, ..
            } => BatchRecord::purge(subject, tombstone.to_json_bytes()?.into(), *expected)
                .with_ttl(writer.marker_ttl().into()),
        })
    }
}

struct Destination {
    entry: EntryKey,
    kv_key: KvKey,
    receipt_key: KvKey,
}

enum Plan {
    Done(WriteOutcome),
    Write(Change),
}

enum Written {
    Committed(WriteReceipt),
    Retry,
}

struct Actor {
    holder: HolderId,
    shards: ShardCount,
    window: RetryWindow,
    writer: KvWriter,
    heartbeat: Heartbeat,
    owner: Option<OwnerId>,
    guard: Option<EntryRevision>,
    entries: HashMap<EntryKey, TrackedEntry>,
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::Receiver<Command>) {
        while let Some(command) = inbox.recv().await {
            match command {
                Command::Submit {
                    intent,
                    enriched,
                    reply,
                } => {
                    let _ = reply.send(self.execute(intent, enriched).await);
                }
                Command::Update {
                    request,
                    topic,
                    key,
                    meta,
                    expected,
                    reply,
                } => {
                    let _ = reply.send(self.update(request, topic, key, meta, expected).await);
                }
                Command::Untrack {
                    request,
                    topic,
                    key,
                    reply,
                } => {
                    let _ = reply.send(self.untrack(request, topic, key).await);
                }
                Command::UntrackAll { reply } => {
                    let _ = reply.send(self.untrack_all().await);
                }
                Command::Entries { reply } => {
                    let _ = reply.send(self.entries.values().cloned().collect());
                }
                Command::Beat { timeout, reply } => {
                    let _ = reply.send(self.beat(timeout).await);
                }
            }
        }
        self.shutdown().await;
    }

    async fn shutdown(&mut self) {
        if let Some(revision) = self.guard.take() {
            if let Err(err) = self.release_guard(revision).await {
                tracing::warn!(error = %err, "tracker could not release its holder guard");
            }
        }
        self.heartbeat.emit(PresenceEvent::Shutdown { holder: self.holder });
        self.heartbeat.release(&self.holder);
    }

    async fn release_guard(&self, revision: EntryRevision) -> Result<(), WriteError> {
        let guard_key = ControlKey::DirectGuard(self.holder).encode()?;
        let mut batch = AtomicBatch::new()?;
        batch.push(
            BatchRecord::purge(
                self.writer.subject(&guard_key),
                Default::default(),
                Expected::At(revision),
            )
            .with_ttl(self.writer.marker_ttl().into()),
        )?;
        self.writer.publish(batch).await?;
        Ok(())
    }

    fn fence(&mut self) {
        tracing::warn!("another owner took the holder guard, dropping every local entry");
        self.entries.clear();
        self.guard = None;
        self.heartbeat.emit(PresenceEvent::Shutdown { holder: self.holder });
    }

    fn fresh_request(&self) -> Result<WriteRequest, EntropyError> {
        Ok(WriteRequest::new(
            OperationId::generate()?,
            self.window.open(UnixMillis::now()),
            self.holder,
        ))
    }

    fn receipt_key(&self, operation: OperationId) -> Result<KvKey, KvKeyError> {
        ControlKey::Receipt(AuthorityId::Direct(self.holder), operation).encode()
    }

    async fn update(
        &mut self,
        request: WriteRequest,
        topic: Topic,
        key: PresenceKey,
        meta: Meta,
        expected: Option<StoredRef>,
    ) -> Result<WriteOutcome, WriteError> {
        let entry = EntryKey::new(topic.clone(), key.clone(), self.holder);
        let Some(tracked) = self.entries.get(&entry) else {
            return Err(WriteError::NotTracked);
        };
        if expected.is_some_and(|expected| &expected != tracked.stored_ref()) {
            return Err(WriteError::Conflict {
                current_ref: tracked.stored_ref().clone(),
                current_meta: tracked.value.meta().clone(),
            });
        }
        let intent = request.update(topic, key, tracked.target(), meta);
        self.execute(intent, None).await
    }

    async fn untrack(
        &mut self,
        request: WriteRequest,
        topic: Topic,
        key: PresenceKey,
    ) -> Result<WriteOutcome, WriteError> {
        let entry = EntryKey::new(topic.clone(), key.clone(), self.holder);
        let Some(tracked) = self.entries.get(&entry) else {
            return Ok(WriteOutcome::Gone);
        };
        let intent = request.untrack(topic, key, tracked.target());
        self.execute(intent, None).await
    }

    async fn untrack_all(&mut self) -> UntrackReport {
        let entries: Vec<EntryKey> = self.entries.keys().cloned().collect();
        let mut report = UntrackReport::default();
        for entry in entries {
            let result = match self.fresh_request() {
                Ok(request) => self.untrack(request, entry.topic().clone(), entry.key().clone()).await,
                Err(err) => Err(err.into()),
            };
            report.outcomes.push(EntryUntrack { entry, result });
        }
        report
    }

    async fn execute(&mut self, intent: WriteIntent, enriched: Option<Meta>) -> Result<WriteOutcome, WriteError> {
        intent.request().window().admit(self.window, UnixMillis::now())?;
        if intent.request().holder() != self.holder {
            return Err(AdmissionError::WrongHolder.into());
        }
        let entry = EntryKey::new(intent.topic().clone(), intent.key().clone(), self.holder);
        let destination = Destination {
            kv_key: entry.encode(self.shards)?,
            receipt_key: self.receipt_key(intent.request().operation_id())?,
            entry,
        };
        let mut sent = false;
        for attempt in 1..=CAS_MAX_ATTEMPTS {
            let receipt_expected = match self.writer.read_receipt(&destination.receipt_key).await? {
                ReceiptRead::Found { revision, receipt } => {
                    return self.replay(&intent, &destination, revision, &receipt, sent).await;
                }
                ReceiptRead::Absent(expected) => expected,
            };
            let guard = self.guard_record().await?;
            let current = self.writer.read_entry(&destination.kv_key).await?;
            let change = match self.plan(&intent, enriched.as_ref(), &destination.entry, current)? {
                Plan::Done(outcome) => return Ok(outcome),
                Plan::Write(change) => change,
            };
            sent = true;
            match self
                .write(&intent, &destination, guard, change, receipt_expected)
                .await?
            {
                Written::Committed(receipt) => return Ok(WriteOutcome::Applied(receipt)),
                Written::Retry => tracing::debug!(attempt, "write did not commit, reading the leader again"),
            }
        }
        Err(WriteError::Contended(CAS_MAX_ATTEMPTS))
    }

    async fn write(
        &mut self,
        intent: &WriteIntent,
        destination: &Destination,
        guard: BatchRecord,
        change: Change,
        receipt_expected: Expected,
    ) -> Result<Written, WriteError> {
        let Destination {
            entry,
            kv_key,
            receipt_key,
        } = destination;
        let data = change.record(&self.writer, kv_key)?;
        let mut batch = AtomicBatch::new()?;
        let guard_position = batch.push(guard)?;
        let receipt_position = batch.next_position();
        let data_position = receipt_position.following();
        let receipt = Receipt::new(intent, receipt_position, vec![change.target(kv_key, data_position)]);
        batch.push(
            BatchRecord::put(
                self.writer.subject(receipt_key),
                receipt.to_json_bytes()?.into(),
                receipt_expected,
            )
            .with_ttl(self.writer.receipt_ttl().into()),
        )?;
        batch.push(data)?;
        match self.writer.publish(batch).await? {
            BatchOutcome::Committed(ack) => {
                self.guard = Some(ack.revision_of(guard_position)?);
                let revision = ack.revision_of(data_position)?;
                let written = receipt.write_receipt(ack.revision_of(receipt_position)?, kv_key)?;
                match change {
                    Change::Put { value, .. } => self.keep(entry.clone(), kv_key.clone(), *value, revision),
                    Change::Purge { .. } => {
                        self.entries.remove(entry);
                    }
                }
                Ok(Written::Committed(written))
            }
            BatchOutcome::Rejected(rejection) if rejection.is_wrong_last_sequence() => {
                self.guard = None;
                Ok(Written::Retry)
            }
            BatchOutcome::Rejected(rejection) => Err(WriteError::Rejected(rejection)),
            BatchOutcome::Unknown => {
                self.guard = None;
                Ok(Written::Retry)
            }
        }
    }

    fn keep(&mut self, entry: EntryKey, kv_key: KvKey, value: StoredValue, revision: EntryRevision) {
        let birth = value
            .birth_rev()
            .or_else(|| {
                self.entries
                    .get(&entry)
                    .filter(|tracked| tracked.lifetime() == value.lifetime())
                    .map(|tracked| tracked.birth)
            })
            .unwrap_or(revision);
        self.entries.insert(
            entry.clone(),
            TrackedEntry {
                entry,
                kv_key,
                value,
                revision,
                birth,
            },
        );
        self.heartbeat.ensure_started();
    }

    fn plan(
        &mut self,
        intent: &WriteIntent,
        enriched: Option<&Meta>,
        entry: &EntryKey,
        current: EntryRead,
    ) -> Result<Plan, WriteError> {
        match (intent.body(), current) {
            (IntentBody::Track { .. }, EntryRead::Live { revision, value }) => {
                let receipt = WriteReceipt::new(
                    value.lifetime(),
                    value.phx_ref().clone(),
                    value.mutation_seq(),
                    revision,
                );
                let kv_key = entry.encode(self.shards)?;
                self.keep(entry.clone(), kv_key, *value, revision);
                Ok(Plan::Done(WriteOutcome::AlreadyLive(receipt)))
            }
            (IntentBody::Track { client_meta }, current) => {
                let meta = enriched.unwrap_or(client_meta).clone();
                let value = StoredValue::born(intent, meta, LifetimeId::generate()?, StoredRef::generate()?)?;
                Ok(Plan::Write(Change::Put {
                    value: Box::new(value),
                    expected: current.expected(),
                }))
            }
            (
                IntentBody::Update { .. } | IntentBody::Untrack { .. },
                EntryRead::Missing | EntryRead::Removed { .. },
            ) => {
                self.entries.remove(entry);
                Ok(Plan::Done(WriteOutcome::Gone))
            }
            (IntentBody::Update { target, client_meta }, EntryRead::Live { revision, value }) => {
                check_target(*target, &value)?;
                let meta = enriched.unwrap_or(client_meta).clone();
                Ok(Plan::Write(Change::Put {
                    value: Box::new(value.successor(intent, meta, revision)?),
                    expected: Expected::At(revision),
                }))
            }
            (IntentBody::Untrack { target }, EntryRead::Live { revision, value }) => {
                check_target(*target, &value)?;
                Ok(Plan::Write(Change::Purge {
                    tombstone: value.retire(intent)?,
                    stored_ref: value.phx_ref().clone(),
                    expected: Expected::At(revision),
                }))
            }
        }
    }

    async fn replay(
        &mut self,
        intent: &WriteIntent,
        destination: &Destination,
        revision: EntryRevision,
        receipt: &Receipt,
        sent: bool,
    ) -> Result<WriteOutcome, WriteError> {
        let Destination { entry, kv_key, .. } = destination;
        if receipt.fingerprint() != intent.fingerprint() {
            return Err(WriteError::OperationConflict);
        }
        let written = receipt.write_receipt(revision, kv_key)?;
        let liveness = match self.writer.read_entry(kv_key).await? {
            EntryRead::Live { revision, value } if value.lifetime() == written.lifetime() => {
                self.keep(entry.clone(), kv_key.clone(), *value, revision);
                Liveness::Live
            }
            _ => {
                if self
                    .entries
                    .get(entry)
                    .is_some_and(|tracked| tracked.lifetime() == written.lifetime())
                {
                    self.entries.remove(entry);
                }
                Liveness::Retired
            }
        };
        if sent {
            Ok(WriteOutcome::Applied(written))
        } else {
            Ok(WriteOutcome::Replayed {
                receipt: written,
                liveness,
            })
        }
    }

    async fn guard_record(&mut self) -> Result<BatchRecord, WriteError> {
        let owner = match self.owner {
            Some(owner) => owner,
            None => *self.owner.insert(OwnerId::generate()?),
        };
        let guard_key = ControlKey::DirectGuard(self.holder).encode()?;
        let expected = match self.guard {
            Some(revision) => Expected::At(revision),
            None => match self.writer.read_guard(&guard_key).await? {
                GuardRead::Held { revision, guard } if guard.owner() == owner => {
                    self.guard = Some(revision);
                    Expected::At(revision)
                }
                GuardRead::Held { .. } => return Err(HolderBusy(self.holder).into()),
                GuardRead::Free(expected) => expected,
            },
        };
        let body = GuardBody::direct(self.holder, owner)
            .to_json_bytes()
            .map_err(StoreError::from)?;
        Ok(BatchRecord::put(self.writer.subject(&guard_key), body.into(), expected)
            .with_ttl(self.writer.guard_ttl().into()))
    }

    async fn beat(&mut self, timeout: Duration) -> BeatReport {
        let mut report = BeatReport::default();
        let keys: Vec<EntryKey> = self.entries.keys().cloned().collect();
        for chunk in keys.chunks(HEARTBEAT_ENTRIES_PER_BATCH) {
            let resolved = match self.beat_chunk(chunk, timeout).await {
                Ok(true) => continue,
                Ok(false) => self.resolve(chunk).await,
                Err(WriteError::HolderBusy(_)) => {
                    self.fence();
                    return report;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "heartbeat batch failed");
                    report.failed = true;
                    continue;
                }
            };
            if let Err(err) = resolved {
                tracing::warn!(error = %err, "heartbeat could not resolve a failed batch");
                report.failed = true;
            }
        }
        report
    }

    async fn beat_chunk(&mut self, chunk: &[EntryKey], timeout: Duration) -> Result<bool, WriteError> {
        let guard = self.guard_record().await?;
        let mut batch = AtomicBatch::new()?;
        let guard_position = batch.push(guard)?;
        let mut staged = Vec::with_capacity(chunk.len());
        for entry in chunk {
            let Some(tracked) = self.entries.get(entry) else {
                continue;
            };
            let value = tracked.value.clone().with_birth(tracked.birth);
            let position = batch.push(
                BatchRecord::put(
                    self.writer.subject(&tracked.kv_key),
                    value.to_json_bytes()?.into(),
                    Expected::At(tracked.revision),
                )
                .with_ttl(self.writer.lease_ttl().into()),
            )?;
            staged.push((entry.clone(), value, position));
        }
        let outcome = match tokio::time::timeout(timeout, self.writer.publish(batch)).await {
            Ok(published) => published?,
            Err(_) => BatchOutcome::Unknown,
        };
        match outcome {
            BatchOutcome::Committed(ack) => {
                self.guard = Some(ack.revision_of(guard_position)?);
                for (entry, value, position) in staged {
                    let revision = ack.revision_of(position)?;
                    if let Some(tracked) = self.entries.get_mut(&entry) {
                        tracked.value = value;
                        tracked.revision = revision;
                    }
                }
                Ok(true)
            }
            BatchOutcome::Rejected(rejection) if rejection.is_wrong_last_sequence() => Ok(false),
            BatchOutcome::Rejected(rejection) => Err(WriteError::Rejected(rejection)),
            BatchOutcome::Unknown => Ok(false),
        }
    }

    async fn resolve(&mut self, chunk: &[EntryKey]) -> Result<(), WriteError> {
        self.guard = None;
        let guard_key = ControlKey::DirectGuard(self.holder).encode()?;
        match self.writer.read_guard(&guard_key).await? {
            GuardRead::Held { guard, .. } if Some(guard.owner()) != self.owner => {
                self.fence();
                return Ok(());
            }
            GuardRead::Held { revision, .. } => self.guard = Some(revision),
            GuardRead::Free(_) => {}
        }
        for entry in chunk {
            let Some(tracked) = self.entries.get(entry).cloned() else {
                continue;
            };
            match self.writer.read_entry(&tracked.kv_key).await? {
                EntryRead::Live { revision, value } if value.lifetime() == tracked.lifetime() => {
                    self.keep(tracked.entry, tracked.kv_key, *value, revision);
                }
                EntryRead::Removed {
                    tombstone: Some(tombstone),
                    ..
                } if tombstone.lifetime() == tracked.lifetime() => {
                    self.entries.remove(entry);
                }
                current => self.retrack(tracked, current.expected()).await?,
            }
        }
        Ok(())
    }

    async fn retrack(&mut self, tracked: TrackedEntry, expected: Expected) -> Result<(), WriteError> {
        tokio::time::sleep(jitter(self.heartbeat.interval())).await;
        let intent = self.fresh_request()?.track(
            tracked.entry.topic().clone(),
            tracked.entry.key().clone(),
            tracked.value.client_meta().clone(),
        );
        let destination = Destination {
            entry: tracked.entry.clone(),
            kv_key: tracked.kv_key.clone(),
            receipt_key: self.receipt_key(intent.request().operation_id())?,
        };
        let value = StoredValue::born(
            &intent,
            tracked.value.meta().clone(),
            LifetimeId::generate()?,
            StoredRef::generate()?,
        )?;
        let guard = self.guard_record().await?;
        let change = Change::Put {
            value: Box::new(value),
            expected,
        };
        let written = match self
            .write(&intent, &destination, guard, change, Expected::Empty)
            .await?
        {
            Written::Committed(written) => written,
            Written::Retry => match self.writer.read_receipt(&destination.receipt_key).await? {
                ReceiptRead::Found { revision, receipt } => {
                    match self.replay(&intent, &destination, revision, &receipt, true).await? {
                        WriteOutcome::Applied(written) => written,
                        _ => return Ok(()),
                    }
                }
                ReceiptRead::Absent(_) => return Err(WriteError::Contended(1)),
            },
        };
        tracing::info!(
            revision = written.revision().get(),
            "heartbeat re-tracked a lost entry with a fresh lifetime"
        );
        self.heartbeat.emit(PresenceEvent::Retracked {
            kv_key: tracked.kv_key,
            old_lifetime: tracked.value.lifetime(),
            old_ref: tracked.value.phx_ref().clone(),
            new_lifetime: written.lifetime(),
            new_ref: written.stored_ref().clone(),
        });
        Ok(())
    }
}

pub(crate) fn check_target(target: EntryTarget, value: &StoredValue) -> Result<(), WriteError> {
    if target.lifetime() != value.lifetime() {
        return Err(WriteError::Conflict {
            current_ref: value.phx_ref().clone(),
            current_meta: value.meta().clone(),
        });
    }
    if target.sequence() != value.mutation_seq() {
        return Err(WriteError::SequenceConflict {
            current: value.mutation_seq(),
        });
    }
    Ok(())
}
