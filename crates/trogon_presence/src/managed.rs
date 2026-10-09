use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::batch::{
    AtomicBatch, BatchBudget, BatchError, BatchOutcome, BatchPublishError, BatchRecord, BatchRevisionError, Unpaced,
};
use crate::clock::{Elapsed, FenceBreach, SelfFence, SelfFenceBound, SuspendAwareClock, SuspendAwareInstant};
use crate::constants::{
    CAS_MAX_ATTEMPTS, CLOCK_SKEW_ALLOWANCE, GUARD_REFRESH_INTERVAL, GUARD_SCHEMA_V1, GUARD_SELF_FENCE,
    KV_OPERATION_DELETE, KV_OPERATION_HEADER, KV_OPERATION_PURGE, KV_SUBJECT_PREFIX, MANAGED_GUARD_CAPACITY,
    MANAGED_GUARD_CAPACITY_MIN, MANAGED_GUARD_HEADER_ALLOWANCE, MANAGED_HEARTBEAT_MAX_ENTRIES, MANAGED_HOLDER_LIMIT,
    MANAGED_RELEASE_MAX_TARGETS, MANAGED_TOPIC_LIMIT, TOKEN_SEPARATOR,
};
use crate::domain::JetStreamRoute;
use crate::entropy::EntropyError;
use crate::error_code::ErrorCode;
use crate::holder::HolderId;
use crate::key::PresenceKey;
use crate::kv_key::{AuthorityId, ControlKey, EntryKey, KvKey, KvKeyError};
use crate::meta::Meta;
use crate::operation::{AdmissionError, IntentBody, LastOp, ReleaseIntent, RetryWindow, UnixMillis, WriteIntent};
use crate::phx_ref::StoredRef;
use crate::position::{LifetimeId, MutationSequence, OperationId, OwnerId, PositionOverflow};
use crate::receipt::{GuardKind, Liveness, Receipt, ReceiptError, ReleasedTarget, SchemaTag, WriteReceipt};
use crate::revision::EntryRevision;
use crate::shard::{fnv1a64, ShardCount};
use crate::store::{ControlRead, EntryRead, Expected, KvWriter, ReceiptRead, StoreError};
use crate::topic::Topic;
use crate::tracker::{check_target, Change, WriteError, WriteOutcome};
use crate::value::{StoredValue, ValueError};
use crate::watch::replay::{RebuildBudget, ReplayConsumer, ReplayError, ReplayFilters};

macro_rules! bounded_count {
    ($name:ident, $error:ident, $default:expr, $min:expr, $what:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(usize);

        #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
        #[error("{} must be at least {}, got {}", $what, $min, .0)]
        pub struct $error(usize);

        impl $name {
            pub fn get(self) -> usize {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }

        impl TryFrom<usize> for $name {
            type Error = $error;

            fn try_from(value: usize) -> Result<Self, Self::Error> {
                if value >= $min {
                    Ok(Self(value))
                } else {
                    Err($error(value))
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

bounded_count!(HolderLimit, HolderLimitError, MANAGED_HOLDER_LIMIT, 1, "holder limit");
bounded_count!(TopicLimit, TopicLimitError, MANAGED_TOPIC_LIMIT, 1, "topic limit");
bounded_count!(
    GuardCapacity,
    GuardCapacityError,
    MANAGED_GUARD_CAPACITY,
    MANAGED_GUARD_CAPACITY_MIN,
    "guard capacity"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ManagedLimits {
    holders: HolderLimit,
    topics: TopicLimit,
    capacity: GuardCapacity,
}

impl ManagedLimits {
    pub fn with_holders(self, holders: HolderLimit) -> Self {
        Self { holders, ..self }
    }

    pub fn with_topics(self, topics: TopicLimit) -> Self {
        Self { topics, ..self }
    }

    pub fn with_capacity(self, capacity: GuardCapacity) -> Self {
        Self { capacity, ..self }
    }

    pub fn holders(self) -> HolderLimit {
        self.holders
    }

    pub fn topics(self) -> TopicLimit {
        self.topics
    }

    pub fn capacity(self) -> GuardCapacity {
        self.capacity
    }
}

#[derive(Debug, Clone)]
enum DeadlineSource {
    Watch(watch::Receiver<Option<SelfFence>>),
    Unbounded,
}

/// Why an owner may no longer act on its cached ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OwnerLapse {
    #[error("the writer tenure ended")]
    Released,
    #[error(transparent)]
    Breach(#[from] FenceBreach),
}

/// The writer lease tenure a coordinator acts under, checked on the suspend-aware clock.
#[derive(Debug, Clone)]
pub struct OwnerDeadline {
    source: DeadlineSource,
    clock: SuspendAwareClock,
}

impl OwnerDeadline {
    pub fn watch(fence: watch::Receiver<Option<SelfFence>>, clock: SuspendAwareClock) -> Self {
        Self {
            source: DeadlineSource::Watch(fence),
            clock,
        }
    }

    pub fn unbounded(clock: SuspendAwareClock) -> Self {
        Self {
            source: DeadlineSource::Unbounded,
            clock,
        }
    }

    pub fn clock(&self) -> &SuspendAwareClock {
        &self.clock
    }

    pub fn check(&self) -> Result<(), OwnerLapse> {
        match &self.source {
            DeadlineSource::Unbounded => Ok(()),
            DeadlineSource::Watch(fence) => match *fence.borrow() {
                Some(fence) => fence.remaining(self.clock.now()).map(drop).map_err(OwnerLapse::from),
                None => Err(OwnerLapse::Released),
            },
        }
    }

    pub fn is_live(&self) -> bool {
        self.check().is_ok()
    }
}

pub type SinkFuture<'a> = Pin<Box<dyn Future<Output = Result<BatchOutcome, BatchPublishError>> + Send + 'a>>;

pub trait BatchSink: Send + Sync {
    fn publish(&self, batch: AtomicBatch) -> SinkFuture<'_>;
}

#[derive(Debug, Clone)]
pub struct NatsBatchSink {
    client: async_nats::Client,
    budget: BatchBudget,
    route: JetStreamRoute,
}

impl NatsBatchSink {
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            budget: BatchBudget::default(),
            route: JetStreamRoute::local(),
        }
    }

    pub fn with_route(self, route: JetStreamRoute) -> Self {
        Self { route, ..self }
    }
}

impl BatchSink for NatsBatchSink {
    fn publish(&self, batch: AtomicBatch) -> SinkFuture<'_> {
        Box::pin(async move {
            batch
                .publish_routed(&self.client, &self.route, self.budget, &Unpaced)
                .await
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManagedError {
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("the request names key {found}, this coordinator owns another key")]
    WrongKey { found: PresenceKey },
    #[error("the key already has {0} live holders")]
    HolderLimit(HolderLimit),
    #[error("the holder already tracks {0} topics on this key")]
    TopicLimit(TopicLimit),
    #[error("the key guard would encode to {size} bytes, the limit is {max}")]
    IdentityCapacityExceeded { size: usize, max: usize },
    #[error("the key coordinator is rebuilding its index")]
    Rebuilding,
    #[error("the writer lease for this key expired, the coordinator fenced itself")]
    Fenced,
    #[error("cached key ownership is no longer valid: {0}")]
    SelfFenced(#[source] FenceBreach),
    #[error("another owner holds the key guard")]
    Superseded,
    #[error("the batch outcome is unknown and no receipt was found")]
    OutcomeUnknown,
    #[error("the key guard could not be decoded: {0}")]
    Guard(#[source] serde_json::Error),
    #[error(transparent)]
    Replay(#[from] ReplayError),
}

macro_rules! via_write_error {
    ($($source:ty),* $(,)?) => {
        $(impl From<$source> for ManagedError {
            fn from(err: $source) -> Self {
                Self::Write(WriteError::from(err))
            }
        })*
    };
}

via_write_error!(
    StoreError,
    BatchError,
    BatchPublishError,
    BatchRevisionError,
    KvKeyError,
    ValueError,
    EntropyError,
    ReceiptError,
    PositionOverflow,
    AdmissionError,
);

impl ManagedError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Write(err) => write_error_code(err),
            Self::WrongKey { .. } => ErrorCode::InvalidKey,
            Self::HolderLimit(_) => ErrorCode::HolderLimit,
            Self::TopicLimit(_) => ErrorCode::TopicLimit,
            Self::IdentityCapacityExceeded { .. } => ErrorCode::IdentityCapacityExceeded,
            Self::Rebuilding | Self::Fenced | Self::SelfFenced(_) | Self::Superseded | Self::Replay(_) => {
                ErrorCode::NotReady
            }
            Self::OutcomeUnknown | Self::Guard(_) => ErrorCode::Unavailable,
        }
    }

    pub fn outcome_unknown(&self) -> bool {
        matches!(self, Self::OutcomeUnknown)
    }
}

pub fn write_error_code(err: &WriteError) -> ErrorCode {
    match err {
        WriteError::Admission(AdmissionError::Expired) => ErrorCode::RetryExpired,
        WriteError::Admission(_) => ErrorCode::InvalidRequest,
        WriteError::OperationConflict => ErrorCode::OperationConflict,
        WriteError::SequenceConflict { .. } => ErrorCode::SequenceConflict,
        WriteError::Conflict { .. } | WriteError::HolderBusy(_) => ErrorCode::Conflict,
        WriteError::NotTracked => ErrorCode::Gone,
        WriteError::Key(KvKeyError::TooLong { .. }) => ErrorCode::KeyTooLong,
        WriteError::Key(_) => ErrorCode::InvalidKey,
        WriteError::Value(ValueError::TooLarge { .. }) => ErrorCode::MetaTooLarge,
        WriteError::Value(_) => ErrorCode::InvalidRequest,
        WriteError::Entropy(_)
        | WriteError::Overflow(_)
        | WriteError::Store(_)
        | WriteError::Batch(_)
        | WriteError::Publish(_)
        | WriteError::Rejected(_)
        | WriteError::Revision(_)
        | WriteError::Receipt(_)
        | WriteError::Contended(_)
        | WriteError::Closed(_) => ErrorCode::Unavailable,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatEntry {
    holder: HolderId,
    topic: Topic,
    lifetime: LifetimeId,
    sequence: MutationSequence,
}

impl BeatEntry {
    pub fn new(holder: HolderId, topic: Topic, lifetime: LifetimeId, sequence: MutationSequence) -> Self {
        Self {
            holder,
            topic,
            lifetime,
            sequence,
        }
    }

    pub fn holder(&self) -> HolderId {
        self.holder
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }

    pub fn sequence(&self) -> MutationSequence {
        self.sequence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatStatus {
    Ok,
    Gone,
    Conflict,
    Unavailable,
}

impl BeatStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Gone => "gone",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseStatus {
    Released(MutationSequence),
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedEntry {
    topic: Topic,
    lifetime: LifetimeId,
    status: ReleaseStatus,
}

impl ReleasedEntry {
    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn lifetime(&self) -> LifetimeId {
        self.lifetime
    }

    pub fn status(&self) -> ReleaseStatus {
        self.status
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOutcome {
    entries: Vec<ReleasedEntry>,
    replayed: bool,
    holder_freed: bool,
}

impl ReleaseOutcome {
    pub fn entries(&self) -> &[ReleasedEntry] {
        &self.entries
    }

    pub fn replayed(&self) -> bool {
        self.replayed
    }

    pub fn holder_freed(&self) -> bool {
        self.holder_freed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
struct TopicDigest(String);

impl TopicDigest {
    fn of(topic: &Topic) -> Self {
        Self(format!("{:016x}", fnv1a64(topic.as_str().as_bytes())))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GuardState {
    Rebuilding,
    Ready,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TopicRow {
    d: TopicDigest,
    l: LifetimeId,
    r: StoredRef,
    s: MutationSequence,
    o: OperationId,
    x: UnixMillis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HolderRow {
    h: HolderId,
    t: Vec<TopicRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedGuardBody {
    v: SchemaTag<GUARD_SCHEMA_V1>,
    kind: GuardKind,
    key: PresenceKey,
    owner: OwnerId,
    state: GuardState,
    holders: Vec<HolderRow>,
}

#[derive(Debug, Clone, PartialEq)]
struct Slot {
    lifetime: LifetimeId,
    stored_ref: StoredRef,
    sequence: MutationSequence,
    op: OperationId,
    expires: UnixMillis,
}

impl Slot {
    fn of(value: &StoredValue, expires: UnixMillis) -> Self {
        Self {
            lifetime: value.lifetime(),
            stored_ref: value.phx_ref().clone(),
            sequence: value.mutation_seq(),
            op: value.last_op().id(),
            expires,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct GuardIndex {
    holders: HashMap<HolderId, HashMap<Topic, Slot>>,
}

impl GuardIndex {
    fn holder_count(&self) -> usize {
        self.holders.len()
    }

    fn topic_count(&self, holder: &HolderId) -> usize {
        self.holders.get(holder).map_or(0, HashMap::len)
    }

    fn contains(&self, holder: &HolderId, topic: &Topic) -> bool {
        self.holders
            .get(holder)
            .is_some_and(|topics| topics.contains_key(topic))
    }

    fn insert(&mut self, holder: HolderId, topic: Topic, slot: Slot) {
        self.holders.entry(holder).or_default().insert(topic, slot);
    }

    fn remove(&mut self, holder: &HolderId, topic: &Topic) {
        if let Some(topics) = self.holders.get_mut(holder) {
            topics.remove(topic);
            if topics.is_empty() {
                self.holders.remove(holder);
            }
        }
    }

    fn renew(&mut self, holder: &HolderId, topic: &Topic, expires: UnixMillis) {
        if let Some(slot) = self.holders.get_mut(holder).and_then(|topics| topics.get_mut(topic)) {
            slot.expires = expires;
        }
    }

    fn stale(&self, now: UnixMillis) -> Vec<(HolderId, Topic, LifetimeId)> {
        self.holders
            .iter()
            .flat_map(|(holder, topics)| {
                topics
                    .iter()
                    .filter(move |(_, slot)| slot.expires.saturating_add(CLOCK_SKEW_ALLOWANCE) < now)
                    .map(move |(topic, slot)| (*holder, topic.clone(), slot.lifetime))
            })
            .collect()
    }

    fn body(&self, key: &PresenceKey, owner: OwnerId, state: GuardState) -> ManagedGuardBody {
        let mut holders: Vec<HolderRow> = self
            .holders
            .iter()
            .map(|(holder, topics)| {
                let mut rows: Vec<TopicRow> = topics
                    .iter()
                    .map(|(topic, slot)| TopicRow {
                        d: TopicDigest::of(topic),
                        l: slot.lifetime,
                        r: slot.stored_ref.clone(),
                        s: slot.sequence,
                        o: slot.op,
                        x: slot.expires,
                    })
                    .collect();
                rows.sort_by(|a, b| a.d.cmp(&b.d));
                HolderRow { h: *holder, t: rows }
            })
            .collect();
        holders.sort_by_key(|row| row.h);
        ManagedGuardBody {
            v: SchemaTag,
            kind: GuardKind::Managed,
            key: key.clone(),
            owner,
            state,
            holders,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum State {
    Rebuilding,
    Ready { guard: EntryRevision, fence: SelfFence },
}

enum Committed<T> {
    Done(T),
    Retry,
}

pub struct KeyCoordinator {
    key: PresenceKey,
    owner: OwnerId,
    shards: ShardCount,
    window: RetryWindow,
    writer: KvWriter,
    sink: Arc<dyn BatchSink>,
    deadline: OwnerDeadline,
    limits: ManagedLimits,
    budget: RebuildBudget,
    guard_key: KvKey,
    state: State,
    index: GuardIndex,
}

impl std::fmt::Debug for KeyCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyCoordinator")
            .field("key", &self.key)
            .field("owner", &self.owner)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

fn revision_after(
    guard_revision: EntryRevision,
    receipt: crate::batch::BatchPosition,
    guard: crate::batch::BatchPosition,
) -> Result<EntryRevision, ManagedError> {
    let distance = receipt.get().saturating_sub(guard.get());
    Ok(guard_revision.checked_add(u64::from(distance))?)
}

fn unix_millis(time: std::time::SystemTime) -> UnixMillis {
    let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    UnixMillis::from(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

impl KeyCoordinator {
    pub(crate) fn new(
        writer: KvWriter,
        shards: ShardCount,
        key: PresenceKey,
        owner: OwnerId,
        deadline: OwnerDeadline,
        limits: ManagedLimits,
    ) -> Result<Self, KvKeyError> {
        let guard_key = ControlKey::WriterGuard(key.clone()).encode()?;
        Ok(Self {
            window: RetryWindow::for_ttl(writer.lease_ttl()),
            sink: Arc::new(NatsBatchSink::new(writer.client().clone()).with_route(writer.route().clone())),
            key,
            owner,
            shards,
            writer,
            deadline,
            limits,
            budget: RebuildBudget::default(),
            guard_key,
            state: State::Rebuilding,
            index: GuardIndex::default(),
        })
    }

    pub fn with_sink(self, sink: Arc<dyn BatchSink>) -> Self {
        Self { sink, ..self }
    }

    pub fn with_rebuild_budget(self, budget: RebuildBudget) -> Self {
        Self { budget, ..self }
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn retry_window(&self) -> RetryWindow {
        self.window
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready { .. })
    }

    pub fn is_idle(&self) -> bool {
        self.index.holder_count() == 0
    }

    pub fn holder_count(&self) -> usize {
        self.index.holder_count()
    }

    pub fn topic_count(&self, holder: &HolderId) -> usize {
        self.index.topic_count(holder)
    }

    fn check_lease(&mut self) -> Result<(), ManagedError> {
        match self.deadline.check() {
            Ok(()) => Ok(()),
            Err(lapse) => {
                tracing::debug!(key = %self.key, %lapse, "writer tenure lapsed, fencing the coordinator");
                self.state = State::Rebuilding;
                Err(ManagedError::Fenced)
            }
        }
    }

    fn check_live(&mut self) -> Result<(), ManagedError> {
        self.check_lease()?;
        if let State::Ready { fence, .. } = self.state {
            if let Err(breach) = fence.remaining(self.deadline.clock().now()) {
                tracing::warn!(key = %self.key, %breach, "key guard ownership lapsed, rebuilding before the next batch");
                self.state = State::Rebuilding;
                return Err(ManagedError::SelfFenced(breach));
            }
        }
        Ok(())
    }

    fn now(&self) -> SuspendAwareInstant {
        self.deadline.clock().now()
    }

    fn adopt(&mut self, guard: EntryRevision, sent: SuspendAwareInstant) -> Result<(), FenceBreach> {
        match SelfFence::confirm(sent, self.now(), SelfFenceBound::from(GUARD_SELF_FENCE)) {
            Ok(fence) => {
                self.state = State::Ready { guard, fence };
                Ok(())
            }
            Err(breach) => {
                tracing::warn!(key = %self.key, %breach, "refusing a guard renewal, ownership must be reacquired");
                self.state = State::Rebuilding;
                Err(breach)
            }
        }
    }

    fn expiry(&self) -> UnixMillis {
        UnixMillis::now().saturating_add(self.writer.lease_ttl().get())
    }

    fn guard_record(
        &self,
        index: &GuardIndex,
        state: GuardState,
        expected: Expected,
    ) -> Result<BatchRecord, ManagedError> {
        let body = serde_json::to_vec(&index.body(&self.key, self.owner, state)).map_err(StoreError::from)?;
        let subject = self.writer.subject(&self.guard_key);
        let size = body.len() + subject.len() + MANAGED_GUARD_HEADER_ALLOWANCE;
        let max = self.limits.capacity.get();
        if size > max {
            return Err(ManagedError::IdentityCapacityExceeded { size, max });
        }
        Ok(BatchRecord::put(subject, body.into(), expected).with_ttl(self.writer.guard_ttl().into()))
    }

    fn ready_guard(&self) -> Result<EntryRevision, ManagedError> {
        match self.state {
            State::Ready { guard, .. } => Ok(guard),
            State::Rebuilding => Err(ManagedError::Rebuilding),
        }
    }

    async fn publish(&mut self, batch: AtomicBatch) -> Result<(BatchOutcome, SuspendAwareInstant), ManagedError> {
        self.check_live()?;
        let sent = self.now();
        let outcome = self.sink.publish(batch).await?;
        if !self.deadline.is_live() {
            tracing::warn!(key = %self.key, "writer lease expired while a batch was in flight");
        }
        Ok((outcome, sent))
    }

    async fn read_guard(&mut self) -> Result<Option<(EntryRevision, ManagedGuardBody)>, ManagedError> {
        let read = self.writer.read_control(&self.guard_key).await?;
        self.check_live()?;
        Ok(match read {
            ControlRead::Free(_) => None,
            ControlRead::Held { revision, payload } => match serde_json::from_slice::<ManagedGuardBody>(&payload) {
                Ok(body) => Some((revision, body)),
                Err(err) => return Err(ManagedError::Guard(err)),
            },
        })
    }

    async fn confirm_guard(&mut self, sent: SuspendAwareInstant) -> Result<(), ManagedError> {
        match self.read_guard().await {
            Ok(Some((revision, body))) if body.owner == self.owner && body.state == GuardState::Ready => {
                self.adopt(revision, sent).map_err(ManagedError::SelfFenced)
            }
            Ok(_) => {
                self.state = State::Rebuilding;
                Err(ManagedError::Superseded)
            }
            Err(err) => {
                self.state = State::Rebuilding;
                Err(err)
            }
        }
    }

    pub async fn ensure_ready(&mut self) -> Result<(), ManagedError> {
        self.check_lease()?;
        let now = self.now();
        match self.state {
            State::Ready { fence, .. } if fence.remaining(now).is_ok() => Ok(()),
            State::Ready { .. } | State::Rebuilding => {
                self.state = State::Rebuilding;
                self.rebuild().await
            }
        }
    }

    pub async fn rebuild(&mut self) -> Result<(), ManagedError> {
        self.state = State::Rebuilding;
        let fenced = self.acquire_rebuilding().await?;
        let index = self.scan().await?;
        self.check_live()?;
        let record = self.guard_record(&index, GuardState::Ready, Expected::At(fenced))?;
        let mut batch = AtomicBatch::new()?;
        let position = batch.push(record)?;
        let (outcome, sent) = self.publish(batch).await?;
        match outcome {
            BatchOutcome::Committed(ack) => {
                self.adopt(ack.revision_of(position)?, sent)
                    .map_err(ManagedError::SelfFenced)?;
                self.index = index;
                tracing::debug!(key = %self.key, holders = self.index.holder_count(), "managed key guard is ready");
                Ok(())
            }
            BatchOutcome::Rejected(rejection) if !rejection.is_wrong_last_sequence() => {
                Err(WriteError::Rejected(rejection).into())
            }
            BatchOutcome::Rejected(_) | BatchOutcome::Unknown => {
                self.confirm_guard(sent).await?;
                self.index = index;
                Ok(())
            }
        }
    }

    async fn acquire_rebuilding(&mut self) -> Result<EntryRevision, ManagedError> {
        for _ in 0..CAS_MAX_ATTEMPTS {
            let expected = match self.writer.read_control(&self.guard_key).await? {
                ControlRead::Free(expected) => expected,
                ControlRead::Held { revision, .. } => Expected::At(revision),
            };
            self.check_live()?;
            let record = self.guard_record(&GuardIndex::default(), GuardState::Rebuilding, expected)?;
            let mut batch = AtomicBatch::new()?;
            let position = batch.push(record)?;
            let (outcome, _) = self.publish(batch).await?;
            match outcome {
                BatchOutcome::Committed(ack) => return Ok(ack.revision_of(position)?),
                BatchOutcome::Rejected(rejection) if !rejection.is_wrong_last_sequence() => {
                    return Err(WriteError::Rejected(rejection).into());
                }
                BatchOutcome::Rejected(_) => continue,
                BatchOutcome::Unknown => {
                    if let Some((revision, body)) = self.read_guard().await? {
                        if body.owner == self.owner && body.state == GuardState::Rebuilding {
                            return Ok(revision);
                        }
                    }
                }
            }
        }
        Err(ManagedError::Rebuilding)
    }

    async fn scan(&mut self) -> Result<GuardIndex, ManagedError> {
        let prefix = format!(
            "{KV_SUBJECT_PREFIX}{TOKEN_SEPARATOR}{}{TOKEN_SEPARATOR}",
            self.writer.bucket().as_str()
        );
        let filter = format!("{prefix}*{TOKEN_SEPARATOR}{}{TOKEN_SEPARATOR}>", self.key.token());
        let mut records = Vec::new();
        let replay = ReplayConsumer::rebuild(
            self.writer.stream(),
            &ReplayFilters::from(filter),
            self.budget,
            |record| records.push(record),
        )
        .await?;
        replay.discard();
        self.check_live()?;
        let lease = self.writer.lease_ttl().get();
        let mut index = GuardIndex::default();
        for record in records {
            let removed = record.headers().is_some_and(|headers| {
                headers.get(async_nats::header::NATS_MARKER_REASON).is_some()
                    || headers
                        .get(KV_OPERATION_HEADER)
                        .is_some_and(|op| op.as_str() == KV_OPERATION_PURGE || op.as_str() == KV_OPERATION_DELETE)
            });
            if removed {
                continue;
            }
            let Some(token) = record.subject().strip_prefix(&prefix) else {
                continue;
            };
            let Some((shard, _)) = token.split_once(TOKEN_SEPARATOR) else {
                continue;
            };
            if self.shards.parse_token(shard).is_err() {
                continue;
            }
            let Ok(kv_key) = KvKey::try_from(token.to_owned()) else {
                continue;
            };
            let Ok(entry) = EntryKey::decode(&kv_key, self.shards) else {
                continue;
            };
            if entry.key() != &self.key {
                continue;
            }
            let Ok(value) = StoredValue::from_json_bytes(record.payload()) else {
                tracing::warn!(key = %self.key, subject = record.subject(), "skipping an undecodable entry during rebuild");
                continue;
            };
            let expires = unix_millis(record.published().get()).saturating_add(lease);
            index.insert(*entry.holder(), entry.topic().clone(), Slot::of(&value, expires));
        }
        Ok(index)
    }

    pub async fn refresh(&mut self) -> Result<(), ManagedError> {
        self.ensure_ready().await?;
        let pruned = self.prune_stale().await?;
        let State::Ready { guard, fence } = self.state else {
            return Err(ManagedError::Rebuilding);
        };
        let fresh = matches!(
            fence.confirmed_at().elapsed_until(self.now()),
            Elapsed::Steady(elapsed) if elapsed < GUARD_REFRESH_INTERVAL
        );
        if !pruned && fresh {
            return Ok(());
        }
        let record = self.guard_record(&self.index, GuardState::Ready, Expected::At(guard))?;
        let mut batch = AtomicBatch::new()?;
        let position = batch.push(record)?;
        let (outcome, sent) = self.publish(batch).await?;
        match outcome {
            BatchOutcome::Committed(ack) => self
                .adopt(ack.revision_of(position)?, sent)
                .map_err(ManagedError::SelfFenced),
            BatchOutcome::Rejected(rejection) if !rejection.is_wrong_last_sequence() => {
                Err(WriteError::Rejected(rejection).into())
            }
            BatchOutcome::Rejected(_) | BatchOutcome::Unknown => self.confirm_guard(sent).await,
        }
    }

    pub async fn relinquish(&mut self) -> Result<(), ManagedError> {
        let State::Ready { guard, .. } = self.state else {
            return Ok(());
        };
        self.state = State::Rebuilding;
        let mut batch = AtomicBatch::new()?;
        batch.push(
            BatchRecord::purge(
                self.writer.subject(&self.guard_key),
                Default::default(),
                Expected::At(guard),
            )
            .with_ttl(self.writer.marker_ttl().into()),
        )?;
        self.publish(batch).await?;
        Ok(())
    }

    async fn prune_stale(&mut self) -> Result<bool, ManagedError> {
        let mut pruned = false;
        for (holder, topic, lifetime) in self.index.stale(UnixMillis::now()) {
            let kv_key = EntryKey::new(topic.clone(), self.key.clone(), holder).encode(self.shards)?;
            let read = match self.writer.read_entry(&kv_key).await {
                Ok(read) => read,
                Err(err) => {
                    tracing::debug!(error = %err, "keeping an uncertain stale entry counted");
                    continue;
                }
            };
            self.check_live()?;
            match read {
                EntryRead::Live { value, .. } if value.lifetime() == lifetime => {
                    let expires = self.expiry();
                    self.index.renew(&holder, &topic, expires);
                }
                _ => {
                    self.index.remove(&holder, &topic);
                    pruned = true;
                }
            }
        }
        Ok(pruned)
    }

    async fn admit_new(&mut self, holder: HolderId, topic: &Topic) -> Result<(), ManagedError> {
        if self.index.contains(&holder, topic) {
            return Ok(());
        }
        let over = |index: &GuardIndex, limits: ManagedLimits| {
            if index.topic_count(&holder) > 0 {
                (index.topic_count(&holder) >= limits.topics.get()).then_some(ManagedError::TopicLimit(limits.topics))
            } else {
                (index.holder_count() >= limits.holders.get()).then_some(ManagedError::HolderLimit(limits.holders))
            }
        };
        if over(&self.index, self.limits).is_none() {
            return Ok(());
        }
        self.prune_stale().await?;
        match over(&self.index, self.limits) {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn receipt_key(&self, operation: OperationId) -> Result<KvKey, KvKeyError> {
        ControlKey::Receipt(AuthorityId::Managed(self.key.clone()), operation).encode()
    }

    pub async fn submit(
        &mut self,
        intent: WriteIntent,
        enriched: Option<Meta>,
        expected_ref: Option<&StoredRef>,
    ) -> Result<WriteOutcome, ManagedError> {
        self.ensure_ready().await?;
        intent.request().window().admit(self.window, UnixMillis::now())?;
        if intent.key() != &self.key {
            return Err(ManagedError::WrongKey {
                found: intent.key().clone(),
            });
        }
        let holder = intent.request().holder();
        let entry = EntryKey::new(intent.topic().clone(), self.key.clone(), holder);
        let kv_key = entry.encode(self.shards)?;
        let receipt_key = self.receipt_key(intent.request().operation_id())?;
        let mut sent = false;
        for attempt in 1..=CAS_MAX_ATTEMPTS {
            let receipt_expected = match self.writer.read_receipt(&receipt_key).await? {
                ReceiptRead::Found { revision, receipt } => {
                    self.check_live()?;
                    return self.replay(&intent, &kv_key, revision, &receipt, sent).await;
                }
                ReceiptRead::Absent(expected) => expected,
            };
            self.check_live()?;
            let current = self.writer.read_entry(&kv_key).await?;
            self.check_live()?;
            let change = match self.plan(&intent, enriched.as_ref(), expected_ref, current).await? {
                Planned::Done(outcome) => return Ok(outcome),
                Planned::Write(change) => change,
            };
            let mut next = self.index.clone();
            match &change {
                Change::Put { value, .. } => {
                    next.insert(holder, intent.topic().clone(), Slot::of(value, self.expiry()))
                }
                Change::Purge { .. } => next.remove(&holder, intent.topic()),
            }
            let guard = self.guard_record(&next, GuardState::Ready, Expected::At(self.ready_guard()?))?;
            let data = change.record(&self.writer, &kv_key)?;
            let mut batch = AtomicBatch::new()?;
            let guard_position = batch.push(guard)?;
            let receipt_position = batch.next_position();
            let data_position = receipt_position.following();
            let receipt = Receipt::new(&intent, receipt_position, vec![change.target(&kv_key, data_position)]);
            batch.push(
                BatchRecord::put(
                    self.writer.subject(&receipt_key),
                    receipt.to_json_bytes()?.into(),
                    receipt_expected,
                )
                .with_ttl(self.writer.receipt_ttl().into()),
            )?;
            batch.push(data)?;
            sent = true;
            let (outcome, sent_at) = self.publish(batch).await?;
            match self.settle(outcome, guard_position, sent_at).await? {
                Committed::Done(guard_revision) => {
                    self.index = next;
                    return Ok(WriteOutcome::Applied(receipt.write_receipt(
                        revision_after(guard_revision, receipt_position, guard_position)?,
                        &kv_key,
                    )?));
                }
                Committed::Retry => {}
            }
            if let Some(resolved) = self.resolve_unknown(&receipt_key, &kv_key, &next, sent_at).await? {
                return Ok(resolved);
            }
            tracing::debug!(attempt, key = %self.key, "managed write did not commit, reading the leader again");
        }
        Err(WriteError::Contended(CAS_MAX_ATTEMPTS).into())
    }

    async fn settle(
        &mut self,
        outcome: BatchOutcome,
        guard_position: crate::batch::BatchPosition,
        sent: SuspendAwareInstant,
    ) -> Result<Committed<EntryRevision>, ManagedError> {
        match outcome {
            BatchOutcome::Committed(ack) => {
                let guard = ack.revision_of(guard_position)?;
                let _ = self.adopt(guard, sent);
                Ok(Committed::Done(guard))
            }
            BatchOutcome::Rejected(rejection) if rejection.is_wrong_last_sequence() => match self.read_guard().await? {
                Some((revision, body))
                    if body.owner == self.owner && self.ready_guard().is_ok_and(|guard| guard == revision) =>
                {
                    Ok(Committed::Retry)
                }
                _ => {
                    self.state = State::Rebuilding;
                    Err(ManagedError::Superseded)
                }
            },
            BatchOutcome::Rejected(rejection) => Err(WriteError::Rejected(rejection).into()),
            BatchOutcome::Unknown => {
                self.state = State::Rebuilding;
                Ok(Committed::Retry)
            }
        }
    }

    async fn resolve_unknown(
        &mut self,
        receipt_key: &KvKey,
        kv_key: &KvKey,
        next: &GuardIndex,
        sent: SuspendAwareInstant,
    ) -> Result<Option<WriteOutcome>, ManagedError> {
        if self.is_ready() {
            return Ok(None);
        }
        match self.writer.read_receipt(receipt_key).await? {
            ReceiptRead::Found { revision, receipt } => {
                self.check_live()?;
                let written = receipt.write_receipt(revision, kv_key)?;
                self.confirm_guard(sent).await?;
                self.index = next.clone();
                Ok(Some(WriteOutcome::Applied(written)))
            }
            ReceiptRead::Absent(_) => Err(ManagedError::OutcomeUnknown),
        }
    }

    async fn plan(
        &mut self,
        intent: &WriteIntent,
        enriched: Option<&Meta>,
        expected_ref: Option<&StoredRef>,
        current: EntryRead,
    ) -> Result<Planned, ManagedError> {
        let holder = intent.request().holder();
        match (intent.body(), current) {
            (IntentBody::Track { .. }, EntryRead::Live { revision, value }) => {
                let receipt = WriteReceipt::new(
                    value.lifetime(),
                    value.phx_ref().clone(),
                    value.mutation_seq(),
                    revision,
                );
                if !self.index.contains(&holder, intent.topic()) {
                    self.admit_new(holder, intent.topic()).await?;
                    let expires = self.expiry();
                    self.index
                        .insert(holder, intent.topic().clone(), Slot::of(&value, expires));
                }
                Ok(Planned::Done(WriteOutcome::AlreadyLive(receipt)))
            }
            (IntentBody::Track { client_meta }, current) => {
                self.admit_new(holder, intent.topic()).await?;
                let meta = enriched.unwrap_or(client_meta).clone();
                let value = StoredValue::born(intent, meta, LifetimeId::generate()?, StoredRef::generate()?)?;
                Ok(Planned::Write(Change::Put {
                    value: Box::new(value),
                    expected: current.expected(),
                }))
            }
            (
                IntentBody::Update { .. } | IntentBody::Untrack { .. },
                EntryRead::Missing | EntryRead::Removed { .. },
            ) => {
                self.index.remove(&holder, intent.topic());
                Ok(Planned::Done(WriteOutcome::Gone))
            }
            (IntentBody::Update { target, client_meta }, EntryRead::Live { revision, value }) => {
                check_target(*target, &value)?;
                if expected_ref.is_some_and(|expected| expected != value.phx_ref()) {
                    return Err(WriteError::Conflict {
                        current_ref: value.phx_ref().clone(),
                        current_meta: value.meta().clone(),
                    }
                    .into());
                }
                let meta = enriched.unwrap_or(client_meta).clone();
                Ok(Planned::Write(Change::Put {
                    value: Box::new(value.successor(intent, meta, revision)?),
                    expected: Expected::At(revision),
                }))
            }
            (IntentBody::Untrack { target }, EntryRead::Live { revision, value }) => {
                check_target(*target, &value)?;
                Ok(Planned::Write(Change::Purge {
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
        kv_key: &KvKey,
        revision: EntryRevision,
        receipt: &Receipt,
        sent: bool,
    ) -> Result<WriteOutcome, ManagedError> {
        if receipt.fingerprint() != intent.fingerprint() {
            return Err(WriteError::OperationConflict.into());
        }
        let written = receipt.write_receipt(revision, kv_key)?;
        let liveness = match self.writer.read_entry(kv_key).await? {
            EntryRead::Live { value, .. } if value.lifetime() == written.lifetime() => Liveness::Live,
            _ => Liveness::Retired,
        };
        self.check_live()?;
        if sent {
            Ok(WriteOutcome::Applied(written))
        } else {
            Ok(WriteOutcome::Replayed {
                receipt: written,
                liveness,
            })
        }
    }

    pub async fn heartbeat(&mut self, entries: &[BeatEntry]) -> Vec<BeatStatus> {
        let mut statuses = vec![BeatStatus::Unavailable; entries.len()];
        if self.ensure_ready().await.is_err() {
            return statuses;
        }
        for (offset, chunk) in entries.chunks(MANAGED_HEARTBEAT_MAX_ENTRIES).enumerate() {
            let base = offset * MANAGED_HEARTBEAT_MAX_ENTRIES;
            for attempt in 1..=CAS_MAX_ATTEMPTS {
                match self.beat_chunk(chunk, &mut statuses[base..base + chunk.len()]).await {
                    Ok(Committed::Done(())) => break,
                    Ok(Committed::Retry) if attempt < CAS_MAX_ATTEMPTS => {}
                    Ok(Committed::Retry) => {
                        statuses[base..base + chunk.len()]
                            .iter_mut()
                            .filter(|status| **status == BeatStatus::Ok)
                            .for_each(|status| *status = BeatStatus::Unavailable);
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, key = %self.key, "heartbeat batch failed");
                        statuses[base..base + chunk.len()]
                            .iter_mut()
                            .filter(|status| **status == BeatStatus::Ok)
                            .for_each(|status| *status = BeatStatus::Unavailable);
                        break;
                    }
                }
            }
        }
        statuses
    }

    async fn beat_chunk(
        &mut self,
        chunk: &[BeatEntry],
        statuses: &mut [BeatStatus],
    ) -> Result<Committed<()>, ManagedError> {
        let mut staged = Vec::new();
        for (slot, entry) in chunk.iter().enumerate() {
            statuses[slot] = BeatStatus::Unavailable;
            let kv_key = match EntryKey::new(entry.topic.clone(), self.key.clone(), entry.holder).encode(self.shards) {
                Ok(kv_key) => kv_key,
                Err(_) => {
                    statuses[slot] = BeatStatus::Gone;
                    continue;
                }
            };
            let read = self.writer.read_entry(&kv_key).await;
            self.check_live()?;
            match read {
                Err(err) => tracing::debug!(error = %err, "heartbeat entry read failed"),
                Ok(EntryRead::Live { revision, value }) if value.lifetime() == entry.lifetime => {
                    if value.mutation_seq() == entry.sequence {
                        staged.push((slot, kv_key, revision, value));
                    } else {
                        statuses[slot] = BeatStatus::Conflict;
                    }
                }
                Ok(EntryRead::Live { .. }) => statuses[slot] = BeatStatus::Gone,
                Ok(EntryRead::Missing | EntryRead::Removed { .. }) => {
                    if self
                        .index
                        .holders
                        .get(&entry.holder)
                        .and_then(|topics| topics.get(&entry.topic))
                        .is_some_and(|slot| slot.lifetime == entry.lifetime)
                    {
                        self.index.remove(&entry.holder, &entry.topic);
                    }
                    statuses[slot] = BeatStatus::Gone;
                }
            }
        }
        if staged.is_empty() {
            return Ok(Committed::Done(()));
        }
        let expires = self.expiry();
        let mut next = self.index.clone();
        for (slot, _, _, value) in &staged {
            let entry = &chunk[*slot];
            if next.contains(&entry.holder, &entry.topic) {
                next.renew(&entry.holder, &entry.topic, expires);
            } else {
                next.insert(entry.holder, entry.topic.clone(), Slot::of(value, expires));
            }
        }
        let guard = self.guard_record(&next, GuardState::Ready, Expected::At(self.ready_guard()?))?;
        let mut batch = AtomicBatch::new()?;
        let guard_position = batch.push(guard)?;
        for (_, kv_key, revision, value) in &staged {
            let renewed = value.as_ref().clone().with_birth(*revision);
            batch.push(
                BatchRecord::put(
                    self.writer.subject(kv_key),
                    renewed.to_json_bytes()?.into(),
                    Expected::At(*revision),
                )
                .with_ttl(self.writer.lease_ttl().into()),
            )?;
        }
        let before = self.ready_guard()?;
        let (outcome, sent) = self.publish(batch).await?;
        let committed = match outcome {
            BatchOutcome::Unknown => match self.read_guard().await? {
                Some((revision, body)) if body.owner == self.owner && revision != before => {
                    let _ = self.adopt(revision, sent);
                    true
                }
                Some((revision, body)) if body.owner == self.owner && revision == before => false,
                _ => {
                    self.state = State::Rebuilding;
                    return Err(ManagedError::Superseded);
                }
            },
            outcome => matches!(self.settle(outcome, guard_position, sent).await?, Committed::Done(_)),
        };
        if !committed {
            return Ok(Committed::Retry);
        }
        self.index = next;
        for (slot, ..) in staged {
            statuses[slot] = BeatStatus::Ok;
        }
        Ok(Committed::Done(()))
    }

    pub async fn release(&mut self, intent: ReleaseIntent) -> Result<ReleaseOutcome, ManagedError> {
        self.ensure_ready().await?;
        intent.request().window().admit(self.window, UnixMillis::now())?;
        if intent.key() != &self.key {
            return Err(ManagedError::WrongKey {
                found: intent.key().clone(),
            });
        }
        if intent.targets().len() > MANAGED_RELEASE_MAX_TARGETS {
            return Err(ManagedError::TopicLimit(self.limits.topics));
        }
        let holder = intent.request().holder();
        let receipt_key = self.receipt_key(intent.request().operation_id())?;
        for attempt in 1..=CAS_MAX_ATTEMPTS {
            let receipt_expected = match self.writer.read_receipt(&receipt_key).await? {
                ReceiptRead::Found { receipt, .. } => {
                    self.check_live()?;
                    return self.replay_release(&intent, &receipt, true);
                }
                ReceiptRead::Absent(expected) => expected,
            };
            self.check_live()?;
            let mut purges = Vec::new();
            let mut released = Vec::with_capacity(intent.targets().len());
            let mut entries = Vec::with_capacity(intent.targets().len());
            let mut next = self.index.clone();
            for target in intent.targets() {
                let kv_key = EntryKey::new(target.topic().clone(), self.key.clone(), holder).encode(self.shards)?;
                let read = self.writer.read_entry(&kv_key).await?;
                self.check_live()?;
                match read {
                    EntryRead::Live { revision, value } if value.lifetime() == target.lifetime() => {
                        let tombstone = value.retire_with(LastOp::release(&intent))?;
                        released.push(Some(ReleasedTarget::new(
                            tombstone.lifetime(),
                            tombstone.mutation_seq(),
                        )));
                        entries.push(ReleasedEntry {
                            topic: target.topic().clone(),
                            lifetime: target.lifetime(),
                            status: ReleaseStatus::Released(tombstone.mutation_seq()),
                        });
                        next.remove(&holder, target.topic());
                        purges.push((
                            kv_key,
                            Change::Purge {
                                tombstone,
                                stored_ref: value.phx_ref().clone(),
                                expected: Expected::At(revision),
                            },
                        ));
                    }
                    EntryRead::Live { value, .. } => {
                        return Err(WriteError::Conflict {
                            current_ref: value.phx_ref().clone(),
                            current_meta: value.meta().clone(),
                        }
                        .into());
                    }
                    EntryRead::Missing | EntryRead::Removed { .. } => {
                        released.push(None);
                        entries.push(ReleasedEntry {
                            topic: target.topic().clone(),
                            lifetime: target.lifetime(),
                            status: ReleaseStatus::Gone,
                        });
                        if next
                            .holders
                            .get(&holder)
                            .and_then(|topics| topics.get(target.topic()))
                            .is_some_and(|slot| slot.lifetime == target.lifetime())
                        {
                            next.remove(&holder, target.topic());
                        }
                    }
                }
            }
            let holder_freed = self.index.topic_count(&holder) > 0 && next.topic_count(&holder) == 0;
            if purges.is_empty() {
                self.index = next;
                return Ok(ReleaseOutcome {
                    entries,
                    replayed: false,
                    holder_freed,
                });
            }
            let guard = self.guard_record(&next, GuardState::Ready, Expected::At(self.ready_guard()?))?;
            let mut batch = AtomicBatch::new()?;
            let guard_position = batch.push(guard)?;
            let receipt = Receipt::release(&intent, batch.next_position(), released);
            batch.push(
                BatchRecord::put(
                    self.writer.subject(&receipt_key),
                    receipt.to_json_bytes()?.into(),
                    receipt_expected,
                )
                .with_ttl(self.writer.receipt_ttl().into()),
            )?;
            for (kv_key, change) in &purges {
                batch.push(change.record(&self.writer, kv_key)?)?;
            }
            let (outcome, sent) = self.publish(batch).await?;
            if let Committed::Done(_) = self.settle(outcome, guard_position, sent).await? {
                self.index = next;
                return Ok(ReleaseOutcome {
                    entries,
                    replayed: false,
                    holder_freed,
                });
            }
            if !self.is_ready() {
                return match self.writer.read_receipt(&receipt_key).await? {
                    ReceiptRead::Found { receipt, .. } => {
                        self.check_live()?;
                        self.confirm_guard(sent).await?;
                        self.index = next;
                        let mut outcome = self.replay_release(&intent, &receipt, false)?;
                        outcome.holder_freed = holder_freed;
                        Ok(outcome)
                    }
                    ReceiptRead::Absent(_) => Err(ManagedError::OutcomeUnknown),
                };
            }
            tracing::debug!(attempt, key = %self.key, "managed release did not commit, reading the leader again");
        }
        Err(WriteError::Contended(CAS_MAX_ATTEMPTS).into())
    }

    fn replay_release(
        &self,
        intent: &ReleaseIntent,
        receipt: &Receipt,
        replayed: bool,
    ) -> Result<ReleaseOutcome, ManagedError> {
        if receipt.fingerprint() != intent.fingerprint() {
            return Err(WriteError::OperationConflict.into());
        }
        let entries = intent
            .targets()
            .iter()
            .zip(receipt.released().iter().chain(std::iter::repeat(&None)))
            .map(|(target, released)| ReleasedEntry {
                topic: target.topic().clone(),
                lifetime: target.lifetime(),
                status: match released {
                    Some(released) => ReleaseStatus::Released(released.sequence()),
                    None => ReleaseStatus::Gone,
                },
            })
            .collect();
        Ok(ReleaseOutcome {
            entries,
            replayed,
            holder_freed: false,
        })
    }
}

enum Planned {
    Done(WriteOutcome),
    Write(Change),
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn slot(byte: u8) -> Slot {
        Slot {
            lifetime: LifetimeId::from([byte; 16]),
            stored_ref: StoredRef::parse("ref").unwrap_or_else(|_| unreachable!()),
            sequence: MutationSequence::FIRST,
            op: OperationId::from([byte; 16]),
            expires: UnixMillis::from(1_000),
        }
    }

    #[test]
    fn removing_the_last_topic_frees_the_holder_slot() -> TestResult {
        let mut index = GuardIndex::default();
        let holder = HolderId::from([1; 16]);
        index.insert(holder, "room:a".parse()?, slot(1));
        index.insert(holder, "room:b".parse()?, slot(2));
        assert_eq!(index.holder_count(), 1);
        index.remove(&holder, &"room:a".parse()?);
        assert_eq!(index.holder_count(), 1);
        index.remove(&holder, &"room:b".parse()?);
        assert_eq!(index.holder_count(), 0);
        Ok(())
    }

    #[test]
    fn guard_body_is_tagged_and_compact() -> TestResult {
        let mut index = GuardIndex::default();
        index.insert(HolderId::from([1; 16]), "room:a".parse()?, slot(1));
        let body = index.body(&"ana".parse()?, OwnerId::from([2; 16]), GuardState::Ready);
        let json: serde_json::Value = serde_json::to_value(&body)?;
        assert_eq!(json["v"], 1);
        assert_eq!(json["kind"], "managed");
        assert_eq!(json["state"], "ready");
        let row = &json["holders"][0]["t"][0];
        assert_eq!(row["d"].as_str().map(str::len), Some(16));
        assert!(row.get("meta").is_none());
        Ok(())
    }

    #[test]
    fn limits_reject_degenerate_values() {
        assert!(HolderLimit::try_from(0).is_err());
        assert!(TopicLimit::try_from(0).is_err());
        assert!(GuardCapacity::try_from(MANAGED_GUARD_CAPACITY_MIN - 1).is_err());
        assert_eq!(HolderLimit::default().get(), 32);
        assert_eq!(TopicLimit::default().get(), 64);
    }
}
