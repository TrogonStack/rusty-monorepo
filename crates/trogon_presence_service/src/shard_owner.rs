use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use std::time::SystemTime;

use async_nats::jetstream::stream::Stream;
use async_nats::{HeaderMap, Message, Subscriber};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;
use trogon_presence::watch::engine::{
    Classified, LiveSet, Observation, RetirementWindow, ShardClassifier, StalePolicy, SweepOutcome, WatchCore,
};
use trogon_presence::watch::replay::{
    RawRecord, RebuildBudget, ReconcileInterval, ReplayConsumer, ReplayError, RetryBackoff, Watermark,
};
use trogon_presence::watch::{BarrierError, BarrierWaiters, ReadBarrier, Verdict, ViewCursor, WaiterPermit};
use trogon_presence::{
    ConnectionId, Diff, DiffSequence, EntryKey, EntryRevision, FenceBreach, GenerationEpoch, HolderId, MetaEntry,
    OwnerEpoch, OwnerId, PresenceKey, Presences, RequestId, Revision, SelfFence, ShardCount, SnapshotId,
    StreamGeneration, SuspendAwareClock, Topic, UnixMillis, ViewShard,
};

use crate::admission::{AdmissionCounters, Counter};
use crate::config::KeepaliveInterval;
use crate::inbox::ReplyInbox;
use crate::lease::{LeaseKey, LeaseStore, Renewal};
use crate::reply::{epoch_headers, ErrorReply, FrameKind, Position, ReplyCode, Response, HEADER_SHARD};
use crate::snapshot::{
    header_wire_len, AssemblyGate, CaptureError, CapturedSnapshot, SnapshotIdentity, SnapshotLimits,
};
use crate::subjects::{
    diff_subject, epoch_subject, parse_snapshot, snapshot_scope, snapshot_shard_filter, ReadOp, SnapshotReplySubject,
};

const KEEPALIVE_RETENTION: Duration = Duration::from_secs(600);
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

pub type OwnedShards = Arc<Mutex<BTreeSet<ViewShard>>>;

pub struct OwnerShared {
    pub client: async_nats::Client,
    pub stream: Stream,
    pub classifier_bucket: trogon_presence::BucketName,
    pub shards: ShardCount,
    pub leases: LeaseStore,
    pub owned: OwnedShards,
    pub snapshot: SnapshotLimits,
    pub gate: AssemblyGate,
    pub(crate) counters: Arc<AdmissionCounters>,
    pub keepalive: KeepaliveInterval,
    pub coalesce: Duration,
    pub stale: StalePolicy,
    pub retirement: RetirementWindow,
    pub rebuild_budget: RebuildBudget,
    pub reconcile: ReconcileInterval,
    pub owner: OwnerId,
    pub generation: StreamGeneration,
    pub clock: SuspendAwareClock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerExit {
    Shutdown,
    LeaseLost,
    Fenced,
    Failed(String),
}

#[derive(Debug, Clone, Copy)]
enum LeaseState {
    Held { fence: SelfFence },
    Lost,
    Fenced(FenceBreach),
}

enum Lapse {
    Lost,
    Fenced(FenceBreach),
}

struct TopicState {
    core: WatchCore,
    cursor: ViewCursor,
    published_since_tick: bool,
    emptied_at: Option<Instant>,
}

impl TopicState {
    fn new(core: WatchCore, epoch: GenerationEpoch) -> Self {
        Self {
            core,
            cursor: ViewCursor::service(epoch),
            published_since_tick: false,
            emptied_at: None,
        }
    }

    fn take_diff(&mut self, now: Instant) -> Option<Diff> {
        if !self.core.has_pending() {
            return None;
        }
        let diff = self.core.flush();
        if diff.is_empty() {
            return None;
        }
        self.published_since_tick = true;
        self.emptied_at = self.core.is_empty().then_some(now);
        Some(diff)
    }

    fn allocate(&mut self) -> Option<(DiffSequence, DiffSequence)> {
        let prev = self.cursor.seq();
        match self.cursor.advance() {
            Ok(next) => {
                self.cursor = next;
                Some((self.cursor.seq(), prev))
            }
            Err(err) => {
                tracing::error!(%err, "diff sequence exhausted, dropping a presence frame");
                None
            }
        }
    }

    fn in_keepalive_set(&self, now: Instant) -> bool {
        !self.core.is_empty()
            || self
                .emptied_at
                .is_some_and(|at| now.saturating_duration_since(at) <= KEEPALIVE_RETENTION)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BarrierRequest {
    generation: StreamGeneration,
    key: PresenceKey,
    holder: HolderId,
    target: EntryRevision,
    expires: UnixMillis,
}

impl BarrierRequest {
    fn barrier(self, topic: Topic) -> ReadBarrier {
        ReadBarrier::new(
            self.generation,
            EntryKey::new(topic, self.key, self.holder),
            self.target,
            self.expires,
        )
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {
    #[serde(default)]
    barrier: Option<BarrierRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRequest {
    request_id: RequestId,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetRequest {
    key: PresenceKey,
    #[serde(default)]
    barrier: Option<BarrierRequest>,
}

enum ReadBody {
    List(ListRequest),
    Get(GetRequest),
}

impl ReadBody {
    fn barrier(&self) -> Option<&BarrierRequest> {
        match self {
            Self::List(body) => body.barrier.as_ref(),
            Self::Get(body) => body.barrier.as_ref(),
        }
    }
}

struct Parked {
    request: Message,
    topic: Topic,
    body: ReadBody,
    barrier: ReadBarrier,
    deadline: Instant,
    _permit: WaiterPermit,
}

#[derive(Serialize)]
struct GetReply<'a> {
    metas: &'a [MetaEntry],
}

#[derive(Serialize)]
struct EpochAnnouncement {
    generation: StreamGeneration,
    owner_epoch: OwnerEpoch,
}

type ShadowView = HashMap<Topic, LiveSet>;

struct Recovery {
    at: Instant,
    backoff: RetryBackoff,
}

impl Recovery {
    fn now() -> Self {
        Self {
            at: Instant::now(),
            backoff: RetryBackoff::default(),
        }
    }

    fn retry(self) -> Self {
        Self {
            at: Instant::now() + self.backoff.jittered(),
            backoff: self.backoff.next(),
        }
    }
}

pub struct ShardOwner {
    shared: Arc<OwnerShared>,
    shard: ViewShard,
    lease_revision: Revision,
    lease: watch::Receiver<LeaseState>,
    keeper: Option<watch::Sender<LeaseState>>,
    epoch: GenerationEpoch,
    classifier: ShardClassifier,
    topics: HashMap<Topic, TopicState>,
    seen: Option<Revision>,
    parked: Vec<Parked>,
    waiters: BarrierWaiters,
}

impl ShardOwner {
    pub fn new(shared: Arc<OwnerShared>, shard: ViewShard, lease_revision: Revision, fence: SelfFence) -> Self {
        let classifier = ShardClassifier::new(&shared.classifier_bucket, shared.shards, shard);
        let epoch = GenerationEpoch::new(shared.generation, OwnerEpoch::new(lease_revision, shared.owner));
        let (keeper, lease) = watch::channel(LeaseState::Held { fence });
        Self {
            shared,
            shard,
            lease_revision,
            lease,
            keeper: Some(keeper),
            epoch,
            classifier,
            topics: HashMap::new(),
            seen: None,
            parked: Vec::new(),
            waiters: BarrierWaiters::process(),
        }
    }

    pub async fn run(mut self, shutdown: watch::Receiver<bool>) -> OwnerExit {
        let Some(lease_tx) = self.keeper.take() else {
            return OwnerExit::Failed("shard owner was already started".to_owned());
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let keeper = tokio::spawn(keep_lease(
            self.shared.leases.clone(),
            self.shard,
            self.lease_revision,
            self.shared.clock.clone(),
            lease_tx,
            stop_rx,
        ));
        let lease = self.lease.clone();
        let exit = self.serve(shutdown, lease).await;
        self.shared
            .owned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.shard);
        let _ = stop_tx.send(true);
        if let Err(err) = keeper.await {
            tracing::warn!(%err, "lease keeper task failed");
        }
        exit
    }

    async fn serve(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
        mut lease: watch::Receiver<LeaseState>,
    ) -> OwnerExit {
        let mut replay = match self.open().await {
            Ok(replay) => Some(replay),
            Err(err) => return OwnerExit::Failed(err.to_string()),
        };
        if let Err(err) = self.announce().await {
            return OwnerExit::Failed(err);
        }
        let (mut lists, mut gets, mut snapshots) = match self.subscribe().await {
            Ok(subscribers) => subscribers,
            Err(err) => return OwnerExit::Failed(err),
        };
        self.shared
            .owned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.shard);
        tracing::info!(
            shard = %self.shared.shards.token(self.shard),
            owner_rev = %self.epoch.epoch().acquired(),
            owner_id = %self.epoch.epoch().owner(),
            "owning shard"
        );

        let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let keepalive_every = self.shared.keepalive.get();
        let mut keepalive = tokio::time::interval_at(Instant::now() + keepalive_every, keepalive_every);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let reconcile_every = self.shared.reconcile.get();
        let mut reconcile = tokio::time::interval_at(Instant::now() + reconcile_every, reconcile_every);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut deadline: Option<Instant> = None;
        let mut recovery: Option<Recovery> = None;
        let exit = loop {
            let remaining = match self.tenure() {
                Ok(remaining) => remaining,
                Err(Lapse::Lost) => break OwnerExit::LeaseLost,
                Err(Lapse::Fenced(breach)) => {
                    tracing::warn!(%breach, shard = %self.shared.shards.token(self.shard), "shard owner self-fenced");
                    break OwnerExit::Fenced;
                }
            };
            tokio::select! {
                _ = shutdown.changed() => break OwnerExit::Shutdown,
                changed = lease.changed() => if changed.is_err() {
                    break OwnerExit::LeaseLost;
                },
                () = tokio::time::sleep(remaining) => {}
                next = next_record(&mut replay) => match next {
                    Ok(record) => self.receive(&record),
                    Err(err) => {
                        tracing::warn!(%err, "shard watch failed, rebuilding");
                        if let Some(failed) = replay.take() {
                            failed.discard();
                        }
                        recovery = Some(Recovery::now());
                    }
                },
                () = wait_until(recovery.as_ref().map(|recovery| recovery.at)) => match self.reopen().await {
                    Ok(fresh) => {
                        replay = Some(fresh);
                        recovery = None;
                        deadline = None;
                        self.flush().await;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "shard watch rebuild failed");
                        recovery = recovery.take().map(Recovery::retry);
                    }
                },
                _ = reconcile.tick(), if replay.is_some() => match self.reopen().await {
                    Ok(fresh) => {
                        if let Some(stale) = replay.replace(fresh) {
                            stale.discard();
                        }
                        deadline = None;
                        self.flush().await;
                    }
                    Err(err) => tracing::warn!(%err, "shard reconcile failed, keeping the current view"),
                },
                () = wait_until(deadline) => {
                    deadline = None;
                    self.flush().await;
                }
                () = wait_until(self.parked.iter().map(|parked| parked.deadline).min()) => {}
                _ = sweep.tick() => self.sweep(replay.as_ref()).await,
                _ = keepalive.tick() => self.keepalive().await,
                Some(request) = lists.next() => self.read(ReadOp::List, request, replay.is_some()).await,
                Some(request) = gets.next() => self.read(ReadOp::Get, request, replay.is_some()).await,
                Some(request) = snapshots.next() => self.snapshot(request, replay.is_some()).await,
            }
            self.settle(replay.is_some()).await;
            if deadline.is_none() && self.topics.values().any(|state| state.core.has_pending()) {
                deadline = Some(Instant::now() + self.shared.coalesce);
            }
        };
        if let Some(replay) = replay {
            replay.discard();
        }
        let _ = lists.unsubscribe().await;
        let _ = gets.unsubscribe().await;
        let _ = snapshots.unsubscribe().await;
        for parked in std::mem::take(&mut self.parked) {
            Response::from(ErrorReply::new(ReplyCode::NotOwner).detail("shard owner is stepping down"))
                .send(&self.shared.client, &parked.request)
                .await;
        }
        if matches!(exit, OwnerExit::Shutdown) {
            self.flush().await;
        }
        tracing::info!(shard = %self.shared.shards.token(self.shard), ?exit, "released shard");
        exit
    }

    async fn consumer(&self) -> Result<(ShadowView, ReplayConsumer), ReplayError> {
        let mut fresh = ShadowView::new();
        let replay = ReplayConsumer::rebuild(
            &self.shared.stream,
            &self.classifier.filters(),
            self.shared.rebuild_budget,
            |record| {
                if let Some((topic, observation)) = self.classify(&record) {
                    fresh.entry(topic).or_default().apply(observation);
                }
            },
        )
        .await?;
        Ok((fresh, replay))
    }

    async fn open(&mut self) -> Result<ReplayConsumer, ReplayError> {
        let (fresh, replay) = self.consumer().await?;
        let epoch = self.epoch;
        self.topics = fresh
            .into_iter()
            .map(|(topic, live)| {
                let mut core = WatchCore::new(live);
                core.flush();
                (topic, TopicState::new(core, epoch))
            })
            .collect();
        self.observe_cores();
        Ok(replay)
    }

    async fn reopen(&mut self) -> Result<ReplayConsumer, ReplayError> {
        let (mut fresh, replay) = self.consumer().await?;
        for (topic, state) in &mut self.topics {
            state.core.reconcile(fresh.remove(topic).unwrap_or_default());
        }
        for (topic, live) in fresh {
            let mut core = WatchCore::default();
            core.reconcile(live);
            self.topics.insert(topic, TopicState::new(core, self.epoch));
        }
        self.observe_cores();
        Ok(replay)
    }

    fn observe_cores(&mut self) {
        let latest = self.topics.values().filter_map(|state| state.core.revision()).max();
        self.seen = self.seen.max(latest);
    }

    fn classify(&self, record: &RawRecord) -> Option<(Topic, Observation)> {
        match self.classifier.classify(record) {
            Classified::Observed(routed) => Some(routed),
            Classified::Rejected { reason, evict } => {
                tracing::warn!(%reason, revision = record.stream_seq().get(), "dropped presence entry");
                evict
            }
        }
    }

    fn receive(&mut self, record: &RawRecord) {
        self.seen = self.seen.max(Some(record.stream_seq()));
        if let Some((topic, observation)) = self.classify(record) {
            let epoch = self.epoch;
            self.topics
                .entry(topic)
                .or_insert_with(|| TopicState::new(WatchCore::default(), epoch))
                .core
                .apply(observation);
        }
    }

    fn tenure(&self) -> Result<Duration, Lapse> {
        match *self.lease.borrow() {
            LeaseState::Held { fence } => fence.remaining(self.shared.clock.now()).map_err(Lapse::Fenced),
            LeaseState::Fenced(breach) => Err(Lapse::Fenced(breach)),
            LeaseState::Lost => Err(Lapse::Lost),
        }
    }

    async fn announce(&mut self) -> Result<(), String> {
        if self.tenure().is_err() {
            return Err("shard ownership lapsed before the epoch announcement".to_owned());
        }
        let mut headers = HeaderMap::new();
        epoch_headers(&mut headers, self.epoch);
        headers.insert(HEADER_SHARD, self.shared.shards.token(self.shard));
        let body = serde_json::to_vec(&EpochAnnouncement {
            generation: self.epoch.generation(),
            owner_epoch: self.epoch.epoch(),
        })
        .map_err(|err| err.to_string())?;
        self.shared
            .client
            .publish_with_headers(epoch_subject(self.shared.shards, self.shard), headers, body.into())
            .await
            .map_err(|err| err.to_string())?;
        let topics: Vec<Topic> = self.topics.keys().cloned().collect();
        for topic in topics {
            let Some(state) = self.topics.get_mut(&topic) else {
                continue;
            };
            if state.core.is_empty() {
                continue;
            }
            state.published_since_tick = true;
            state.emptied_at = None;
            let seq = state.cursor.seq();
            let position = self.position(seq, Some(seq));
            self.publish(&topic, position, FrameKind::Keepalive, &empty_diff())
                .await;
        }
        Ok(())
    }

    async fn subscribe(&self) -> Result<(Subscriber, Subscriber, Subscriber), String> {
        let client = &self.shared.client;
        let lists = client
            .subscribe(ReadOp::List.shard_filter(self.shared.shards, self.shard))
            .await
            .map_err(|err| err.to_string())?;
        let gets = client
            .subscribe(ReadOp::Get.shard_filter(self.shared.shards, self.shard))
            .await
            .map_err(|err| err.to_string())?;
        let snapshots = client
            .subscribe(snapshot_shard_filter(self.shared.shards, self.shard))
            .await
            .map_err(|err| err.to_string())?;
        client.flush().await.map_err(|err| err.to_string())?;
        Ok((lists, gets, snapshots))
    }

    fn position(&self, seq: DiffSequence, prev: Option<DiffSequence>) -> Position {
        Position {
            epoch: self.epoch,
            shards: self.shared.shards,
            shard: self.shard,
            seq,
            prev,
        }
    }

    async fn publish(&self, topic: &Topic, position: Position, kind: FrameKind, diff: &Diff) {
        if self.tenure().is_err() {
            tracing::debug!("shard ownership lapsed, refusing a broadcast");
            return;
        }
        let body = match serde_json::to_vec(diff) {
            Ok(body) => body,
            Err(err) => {
                tracing::warn!(%err, "could not encode a presence frame");
                return;
            }
        };
        let headers = position.headers(topic, kind);
        if header_wire_len(&headers) + body.len() > self.shared.snapshot.payload().get() {
            tracing::warn!(
                bytes = body.len(),
                "presence frame exceeds the payload budget, readers recover through the gap"
            );
            return;
        }
        if let Err(err) = self
            .shared
            .client
            .publish_with_headers(diff_subject(topic), headers, body.into())
            .await
        {
            tracing::warn!(%err, "could not publish a presence frame");
        }
    }

    async fn emit(&mut self, topic: &Topic, diff: Diff) {
        let room = self.diff_room(topic);
        for piece in split_diff(diff, room) {
            let Some((seq, prev)) = self.topics.get_mut(topic).and_then(TopicState::allocate) else {
                return;
            };
            let position = self.position(seq, Some(prev));
            self.publish(topic, position, FrameKind::Diff, &piece).await;
        }
    }

    fn diff_room(&self, topic: &Topic) -> usize {
        let widest = DiffSequence::from(u64::MAX);
        let headers = self.position(widest, Some(widest)).headers(topic, FrameKind::Diff);
        self.shared
            .snapshot
            .payload()
            .get()
            .saturating_sub(header_wire_len(&headers))
    }

    async fn flush(&mut self) {
        let now = Instant::now();
        let diffs: Vec<_> = self
            .topics
            .iter_mut()
            .filter_map(|(topic, state)| state.take_diff(now).map(|diff| (topic.clone(), diff)))
            .collect();
        for (topic, diff) in diffs {
            self.emit(&topic, diff).await;
        }
    }

    async fn flush_topic(&mut self, topic: &Topic) {
        let diff = self
            .topics
            .get_mut(topic)
            .and_then(|state| state.take_diff(Instant::now()));
        if let Some(diff) = diff {
            self.emit(topic, diff).await;
        }
    }

    async fn sweep(&mut self, replay: Option<&ReplayConsumer>) {
        let now = SystemTime::now();
        let policy = self.shared.stale;
        for state in self.topics.values_mut() {
            state.core.forget_retired(now, self.shared.retirement);
        }
        if !self.topics.values().any(|state| state.core.has_stale(now, policy)) {
            return;
        }
        let watermark = match replay {
            Some(replay) => replay.watermark().await,
            None => Watermark::Unknown,
        };
        for state in self.topics.values_mut() {
            match state.core.sweep(watermark, now, policy) {
                SweepOutcome::Suspended(watermark) => {
                    tracing::debug!(?watermark, "shard sweep suspended until the replay catches up");
                    return;
                }
                SweepOutcome::Swept(0) => {}
                SweepOutcome::Swept(swept) => {
                    tracing::warn!(swept, "swept presence entries that never received an expiry marker");
                }
            }
        }
    }

    async fn keepalive(&mut self) {
        let now = Instant::now();
        let mut frames = Vec::new();
        self.topics
            .retain(|_, state| state.in_keepalive_set(now) || state.core.has_pending());
        for (topic, state) in &mut self.topics {
            if state.in_keepalive_set(now) && !state.published_since_tick {
                frames.push((topic.clone(), state.cursor.seq()));
            }
            state.published_since_tick = false;
        }
        let empty = empty_diff();
        for (topic, seq) in frames {
            let position = self.position(seq, Some(seq));
            self.publish(&topic, position, FrameKind::Keepalive, &empty).await;
        }
    }

    fn read_target(&self, op: ReadOp, request: &Message) -> Result<Topic, Response> {
        let target = op
            .parse(&request.subject, self.shared.shards)
            .map_err(|err| Response::from(ErrorReply::new(err.code()).detail(err)))?;
        let owner = ViewShard::of(target.topic(), self.shared.shards);
        if owner != self.shard || target.addressed() != Some(self.shard) {
            return Err(ErrorReply::not_owner(self.shared.shards, owner).into());
        }
        Ok(target.topic().clone())
    }

    fn read_headers(&self, topic: &Topic, kind: FrameKind) -> HeaderMap {
        let seq = self
            .topics
            .get(topic)
            .map_or(DiffSequence::FIRST, |state| state.cursor.seq());
        self.position(seq, None).headers(topic, kind)
    }

    async fn read(&mut self, op: ReadOp, request: Message, ready: bool) {
        let topic = match self.read_target(op, &request) {
            Ok(topic) => topic,
            Err(response) => return response.send(&self.shared.client, &request).await,
        };
        let parsed = match op {
            ReadOp::List => parse_body(&request.payload).map(ReadBody::List),
            ReadOp::Get => parse_required(&request.payload).map(ReadBody::Get),
        };
        let body = match parsed {
            Ok(body) => body,
            Err(response) => return response.send(&self.shared.client, &request).await,
        };
        let Some(barrier) = body.barrier().cloned() else {
            let response = if ready {
                self.answer(&topic, &body).await
            } else {
                rebuilding()
            };
            return response.send(&self.shared.client, &request).await;
        };
        let barrier = barrier.barrier(topic.clone());
        if let Err(response) = self.park(request.clone(), topic, body, barrier) {
            response.send(&self.shared.client, &request).await;
        }
    }

    fn park(&mut self, request: Message, topic: Topic, body: ReadBody, barrier: ReadBarrier) -> Result<(), Response> {
        barrier
            .admit(self.shared.generation, &topic)
            .map_err(|err| Response::from(ErrorReply::from(err)))?;
        let budget = barrier
            .wait_budget(UnixMillis::now())
            .map_err(|err| Response::from(ErrorReply::from(err)))?;
        let permit = self
            .waiters
            .acquire(&topic)
            .map_err(|err| Response::from(ErrorReply::from(err)))?;
        self.parked.push(Parked {
            request,
            topic,
            body,
            barrier,
            deadline: Instant::now() + budget,
            _permit: permit,
        });
        Ok(())
    }

    async fn settle(&mut self, ready: bool) {
        if self.parked.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut resolved = Vec::new();
        let mut waiting = Vec::new();
        for parked in std::mem::take(&mut self.parked) {
            let verdict = if ready { self.verdict(&parked) } else { Verdict::Pending };
            match verdict {
                Verdict::Satisfied => resolved.push((parked, None)),
                Verdict::Unavailable => {
                    let err = parked.barrier.unavailable();
                    resolved.push((parked, Some(err)));
                }
                Verdict::Pending if parked.deadline <= now => {
                    let err = if ready {
                        parked.barrier.expired()
                    } else {
                        BarrierError::NotReady
                    };
                    resolved.push((parked, Some(err)));
                }
                Verdict::Pending => waiting.push(parked),
            }
        }
        self.parked = waiting;
        for (parked, failure) in resolved {
            let response = match failure {
                Some(err) => ErrorReply::from(err).into(),
                None => self.answer(&parked.topic, &parked.body).await,
            };
            response.send(&self.shared.client, &parked.request).await;
        }
    }

    fn verdict(&mut self, parked: &Parked) -> Verdict {
        let seen = self.seen;
        let epoch = self.epoch;
        let state = self
            .topics
            .entry(parked.topic.clone())
            .or_insert_with(|| TopicState::new(WatchCore::default(), epoch));
        if let Some(seen) = seen {
            state.core.advance(seen);
        }
        parked.barrier.verdict(&state.core)
    }

    async fn answer(&mut self, topic: &Topic, body: &ReadBody) -> Response {
        self.flush_topic(topic).await;
        match body {
            ReadBody::List(_) => self.list_response(topic),
            ReadBody::Get(body) => self.get_response(topic, body),
        }
    }

    fn list_response(&self, topic: &Topic) -> Response {
        let presences = self
            .topics
            .get(topic)
            .map(|state| state.core.presences())
            .unwrap_or_default();
        match serde_json::to_vec(&presences) {
            Ok(encoded) => self.bounded(self.read_headers(topic, FrameKind::State), encoded),
            Err(err) => ErrorReply::new(ReplyCode::Unavailable).detail(err).into(),
        }
    }

    fn bounded(&self, headers: HeaderMap, body: Vec<u8>) -> Response {
        if header_wire_len(&headers) + body.len() <= self.shared.snapshot.payload().get() {
            return Response::ok_with(headers, body);
        }
        ErrorReply::new(ReplyCode::SnapshotTooLarge)
            .detail("state exceeds the payload budget, request a snapshot instead")
            .into()
    }

    fn get_response(&self, topic: &Topic, body: &GetRequest) -> Response {
        let metas = self
            .topics
            .get(topic)
            .map(|state| state.core.metas_for(&body.key))
            .unwrap_or_default();
        let headers = self.read_headers(topic, FrameKind::State);
        match serde_json::to_vec(&GetReply { metas: &metas }) {
            Ok(encoded) => self.bounded(headers, encoded),
            Err(err) => ErrorReply::new(ReplyCode::Unavailable).detail(err).into(),
        }
    }
}

impl ShardOwner {
    async fn snapshot(&mut self, request: Message, ready: bool) {
        let shared = self.shared.clone();
        let inbox = request.reply.clone().and_then(|reply| {
            let (key, connection) = snapshot_scope(request.subject.as_str())?;
            ReplyInbox::under_connection_token(key, connection, reply).ok()
        });
        let Some(inbox) = inbox else {
            shared.counters.bump(Counter::RejectedInbox);
            tracing::debug!("dropping a snapshot request with an unscoped reply inbox");
            return;
        };
        match self.capture(&request, ready).await {
            Ok((captured, reply, connection)) => {
                let permit = match shared.gate.admit(&connection, captured.manifest().total_bytes()) {
                    Ok(permit) => permit,
                    Err(refusal) => {
                        return Response::from(ErrorReply::new(ReplyCode::Overloaded).detail(refusal))
                            .send_to(&shared.client, inbox.into_subject())
                            .await;
                    }
                };
                let headers = manifest_headers(&captured);
                let manifest = match serde_json::to_vec(captured.manifest()) {
                    Ok(manifest) => manifest,
                    Err(err) => {
                        return Response::from(ErrorReply::new(ReplyCode::Unavailable).detail(err))
                            .send_to(&shared.client, inbox.into_subject())
                            .await;
                    }
                };
                tokio::spawn(async move {
                    Response::ok_with(headers, manifest)
                        .send_to(&shared.client, inbox.into_subject())
                        .await;
                    if let Err(err) = captured.deliver(&shared.client, &reply).await {
                        tracing::warn!(%err, "could not deliver a snapshot");
                    }
                    drop(permit);
                });
            }
            Err(response) => response.send_to(&shared.client, inbox.into_subject()).await,
        }
    }

    async fn capture(
        &mut self,
        request: &Message,
        ready: bool,
    ) -> Result<(CapturedSnapshot, SnapshotReplySubject, ConnectionId), Response> {
        let shards = self.shared.shards;
        let target = parse_snapshot(&request.subject, shards)
            .map_err(|err| Response::from(ErrorReply::new(err.code()).detail(err)))?;
        let topic = target.read().topic().clone();
        let owner = ViewShard::of(&topic, shards);
        if owner != self.shard || target.read().addressed() != Some(self.shard) {
            return Err(ErrorReply::not_owner(shards, owner).into());
        }
        if !ready {
            return Err(rebuilding());
        }
        let body: SnapshotRequest = parse_required(&request.payload)?;
        self.flush_topic(&topic).await;
        if self.tenure().is_err() || self.epoch.generation() != self.shared.generation {
            return Err(ErrorReply::new(ReplyCode::NotOwner)
                .detail("shard ownership is no longer current")
                .into());
        }
        let (presences, seq) = self
            .topics
            .get(&topic)
            .map_or((Presences::default(), DiffSequence::FIRST), |state| {
                (state.core.presences(), state.cursor.seq())
            });
        let snapshot = SnapshotId::generate()
            .map_err(|err| Response::from(ErrorReply::new(ReplyCode::Unavailable).detail(err)))?;
        let identity = SnapshotIdentity::new(body.request_id, snapshot, self.epoch, seq);
        let captured = CapturedSnapshot::capture(identity, &presences, self.shared.snapshot).map_err(|err| {
            let code = match err {
                CaptureError::Encode(_) => ReplyCode::Unavailable,
                _ => ReplyCode::SnapshotTooLarge,
            };
            Response::from(ErrorReply::new(code).detail(err))
        })?;
        let reply = SnapshotReplySubject::new(target.read().caller(), target.connection(), &snapshot);
        Ok((captured, reply, *target.connection()))
    }
}

fn manifest_headers(captured: &CapturedSnapshot) -> HeaderMap {
    let identity = captured.manifest().identity();
    let mut headers = HeaderMap::new();
    epoch_headers(&mut headers, identity.epoch());
    headers.insert(crate::reply::HEADER_SEQ, identity.seq().to_string());
    headers.insert(crate::reply::HEADER_KIND, FrameKind::Manifest.as_str());
    headers
}

fn empty_diff() -> Diff {
    Diff::new(Presences::default(), Presences::default())
}

fn split_diff(diff: Diff, room: usize) -> Vec<Diff> {
    if serde_json::to_vec(&diff).is_ok_and(|body| body.len() <= room) {
        return vec![diff];
    }
    let (joins, leaves) = diff.into_parts();
    let mut joins = joins.into_map();
    let mut leaves = leaves.into_map();
    let keys: BTreeSet<PresenceKey> = joins.keys().chain(leaves.keys()).cloned().collect();
    let mut pieces = Vec::new();
    let mut current = (BTreeMap::new(), BTreeMap::new());
    let mut used: usize = 0;
    for key in keys {
        let group = (
            joins.remove(&key).map(|metas| (key.clone(), metas)),
            leaves.remove(&key).map(|metas| (key.clone(), metas)),
        );
        let single = Diff::new(
            group.0.iter().cloned().collect::<BTreeMap<_, _>>().into(),
            group.1.iter().cloned().collect::<BTreeMap<_, _>>().into(),
        );
        let size = serde_json::to_vec(&single).map_or(usize::MAX, |body| body.len());
        if used > 0 && used.saturating_add(size) > room {
            let (joins, leaves) = std::mem::take(&mut current);
            pieces.push(Diff::new(Presences::from(joins), Presences::from(leaves)));
            used = 0;
        }
        if let Some((key, metas)) = group.0 {
            current.0.insert(key, metas);
        }
        if let Some((key, metas)) = group.1 {
            current.1.insert(key, metas);
        }
        used = used.saturating_add(size);
    }
    if !current.0.is_empty() || !current.1.is_empty() {
        pieces.push(Diff::new(Presences::from(current.0), Presences::from(current.1)));
    }
    pieces
}

pub fn parse_body<T: for<'de> Deserialize<'de> + Default>(payload: &[u8]) -> Result<T, Response> {
    if payload.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    parse_required(payload)
}

pub fn parse_required<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, Response> {
    serde_json::from_slice(payload).map_err(|err| ErrorReply::new(ReplyCode::InvalidRequest).detail(err).into())
}

fn rebuilding() -> Response {
    ErrorReply::new(ReplyCode::NotReady)
        .detail("shard view is rebuilding")
        .into()
}

async fn next_record(replay: &mut Option<ReplayConsumer>) -> Result<RawRecord, ReplayError> {
    match replay {
        Some(replay) => replay.next().await,
        None => std::future::pending().await,
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn keep_lease(
    leases: LeaseStore,
    shard: ViewShard,
    mut revision: Revision,
    clock: SuspendAwareClock,
    state: watch::Sender<LeaseState>,
    mut stop: watch::Receiver<bool>,
) {
    let every = leases.ttl().renew_every();
    let mut tick = tokio::time::interval_at(Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = tick.tick() => {
                let LeaseState::Held { fence } = *state.borrow() else {
                    break;
                };
                let remaining = match fence.remaining(clock.now()) {
                    Ok(remaining) => remaining,
                    Err(breach) => {
                        let _ = state.send(LeaseState::Fenced(breach));
                        break;
                    }
                };
                let sent = clock.now();
                let outcome = tokio::select! {
                    outcome = leases.renew(LeaseKey::View(shard), revision) => outcome,
                    () = tokio::time::sleep(remaining) => {
                        let _ = state.send(LeaseState::Fenced(fence.lapse(clock.now())));
                        break;
                    }
                };
                match outcome {
                    Ok(Renewal::Renewed(next)) => {
                        revision = next;
                        match fence.renew(sent, clock.now()) {
                            Ok(renewed) => {
                                let _ = state.send(LeaseState::Held { fence: renewed });
                            }
                            Err(breach) => {
                                tracing::warn!(%breach, "refusing a shard lease renewal, ownership must be reacquired");
                                let _ = state.send(LeaseState::Fenced(breach));
                                break;
                            }
                        }
                    }
                    Ok(Renewal::Lost) => {
                        let _ = state.send(LeaseState::Lost);
                        return;
                    }
                    Ok(Renewal::Unknown) => tracing::warn!("lease renewal outcome unknown"),
                    Err(err) => tracing::warn!(%err, "lease renewal failed"),
                }
            }
        }
    }
    match leases.release(LeaseKey::View(shard), revision).await {
        Ok(true) => {}
        Ok(false) => tracing::warn!("lease changed before release"),
        Err(err) => tracing::warn!(%err, "lease release failed"),
    }
}
