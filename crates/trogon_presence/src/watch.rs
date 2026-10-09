mod barrier;
mod cursor;
pub mod engine;
mod presences;
pub mod replay;

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use async_nats::jetstream::stream::Stream;
use tokio::sync::{broadcast, mpsc, oneshot, watch as signal};
use tokio::task::JoinHandle;

use crate::config::PresenceConfig;
use crate::constants::BARRIER_WAITERS_PER_PROCESS;
use crate::entropy::EntropyError;
use crate::key::PresenceKey;
use crate::operation::UnixMillis;
use crate::position::{DiffSequence, LocalViewId, StreamGeneration, ViewIncarnation};
use crate::revision::Revision;
use crate::topic::Topic;

pub use self::barrier::{BarrierError, BarrierWaiters, ReadBarrier, Verdict, WaiterPermit};
pub use self::cursor::{CursorStep, ViewCursor};
use self::engine::{
    Classified, Classifier, LiveSet, Observation, RetirementWindow, StalePolicy, SweepOutcome, WatchCore,
};
pub use self::engine::{FetchEntry, FetchError};
pub use self::presences::{Diff, MetaEntry, Presences};
use self::replay::{
    AuditVerdict, RawRecord, RebuildBudget, ReconcileInterval, ReconnectAudit, ReplayConsumer, ReplayError,
    RetryBackoff,
};

const DEFAULT_COALESCE_WINDOW: Duration = Duration::from_millis(100);
const DIFF_BUFFER: usize = 256;
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoalesceWindow(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("coalesce window must be greater than zero")]
pub struct CoalesceWindowError;

impl CoalesceWindow {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for CoalesceWindow {
    fn default() -> Self {
        Self(DEFAULT_COALESCE_WINDOW)
    }
}

impl TryFrom<Duration> for CoalesceWindow {
    type Error = CoalesceWindowError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(CoalesceWindowError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchOptions {
    window: CoalesceWindow,
    budget: RebuildBudget,
    reconcile: ReconcileInterval,
}

impl WatchOptions {
    pub fn with_window(self, window: CoalesceWindow) -> Self {
        Self { window, ..self }
    }

    pub fn with_budget(self, budget: RebuildBudget) -> Self {
        Self { budget, ..self }
    }

    pub fn with_reconcile(self, reconcile: ReconcileInterval) -> Self {
        Self { reconcile, ..self }
    }

    pub fn window(self) -> CoalesceWindow {
        self.window
    }

    pub fn budget(self) -> RebuildBudget {
        self.budget
    }

    pub fn reconcile(self) -> ReconcileInterval {
        self.reconcile
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Readiness {
    Ready,
    Rebuilding,
}

pub trait MetaFetcher: Send + Sync + 'static {
    fn fetch(
        &self,
        topic: &Topic,
        entries: Vec<FetchEntry>,
    ) -> impl Future<Output = Result<Vec<FetchEntry>, FetchError>> + Send {
        let _ = topic;
        std::future::ready(Ok(entries))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopFetcher;

impl MetaFetcher for NoopFetcher {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchCounters {
    rejected: u64,
    swept: u64,
    suspended: u64,
    reconnects: u64,
    reconciles: u64,
}

impl WatchCounters {
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    pub fn swept(&self) -> u64 {
        self.swept
    }

    pub fn suspended(&self) -> u64 {
        self.suspended
    }

    pub fn reconnects(&self) -> u64 {
        self.reconnects
    }

    pub fn reconciles(&self) -> u64 {
        self.reconciles
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("watch did not become ready: {0}")]
    NotReady(ReplayError),
    #[error("watch replay failed: {0}")]
    Replay(ReplayError),
    #[error(transparent)]
    Entropy(#[from] EntropyError),
}

impl From<ReplayError> for WatchError {
    fn from(err: ReplayError) -> Self {
        if err.is_not_ready() {
            Self::NotReady(err)
        } else {
            Self::Replay(err)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewDiff {
    cursor: ViewCursor,
    prev: DiffSequence,
    diff: Diff,
}

impl ViewDiff {
    pub fn cursor(&self) -> ViewCursor {
        self.cursor
    }

    pub fn prev(&self) -> DiffSequence {
        self.prev
    }

    pub fn diff(&self) -> &Diff {
        &self.diff
    }

    pub fn into_diff(self) -> Diff {
        self.diff
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewSnapshot {
    presences: Presences,
    cursor: ViewCursor,
}

impl ViewSnapshot {
    pub fn presences(&self) -> &Presences {
        &self.presences
    }

    pub fn cursor(&self) -> ViewCursor {
        self.cursor
    }

    pub fn into_presences(self) -> Presences {
        self.presences
    }
}

struct BarrierRequest {
    barrier: ReadBarrier,
    reply: oneshot::Sender<Result<ViewSnapshot, BarrierError>>,
}

struct Snapshot {
    presences: Presences,
    revision: Revision,
    cursor: ViewCursor,
}

struct Shared {
    state: RwLock<Snapshot>,
    revision: signal::Sender<Revision>,
    readiness: signal::Sender<Readiness>,
    diffs: broadcast::Sender<ViewDiff>,
    rejected: AtomicU64,
    swept: AtomicU64,
    suspended: AtomicU64,
    reconnects: AtomicU64,
    reconciles: AtomicU64,
}

pub struct TopicWatch {
    topic: Topic,
    generation: StreamGeneration,
    shared: Arc<Shared>,
    requests: mpsc::Sender<BarrierRequest>,
    waiters: BarrierWaiters,
    task: JoinHandle<()>,
}

impl TopicWatch {
    pub(crate) async fn start<F: MetaFetcher>(
        stream: Stream,
        audit: ReconnectAudit,
        config: &PresenceConfig,
        generation: StreamGeneration,
        topic: Topic,
        options: WatchOptions,
        fetcher: F,
    ) -> Result<Self, WatchError> {
        let view = LocalViewId::generate()?;
        let cursor = ViewCursor::local(view, ViewIncarnation::FIRST);
        let (diffs, _) = broadcast::channel(DIFF_BUFFER);
        let (revision, _) = signal::channel(Revision::default());
        let (readiness, _) = signal::channel(Readiness::Ready);
        let shared = Arc::new(Shared {
            state: RwLock::new(Snapshot {
                presences: Presences::default(),
                revision: Revision::default(),
                cursor,
            }),
            revision,
            readiness,
            diffs,
            rejected: AtomicU64::new(0),
            swept: AtomicU64::new(0),
            suspended: AtomicU64::new(0),
            reconnects: AtomicU64::new(0),
            reconciles: AtomicU64::new(0),
        });
        let source = Source {
            stream,
            classifier: Classifier::new(config.bucket(), config.shards(), topic.clone()),
            budget: options.budget(),
        };
        let (live, replay) = source.rebuild(&shared).await?;
        let (requests, inbox) = mpsc::channel(BARRIER_WAITERS_PER_PROCESS);
        let mut runner = Runner {
            topic: topic.clone(),
            source,
            core: WatchCore::new(live),
            shared: Arc::clone(&shared),
            fetcher,
            window: options.window(),
            reconcile: options.reconcile(),
            stale: StalePolicy::from(config),
            retirement: RetirementWindow::from(config),
            view,
            incarnation: ViewIncarnation::FIRST,
            cursor,
            inbox,
            pending: Vec::new(),
            audit,
        };
        runner.render().await;
        runner.publish(None);
        let task = tokio::spawn(runner.run(replay));
        Ok(Self {
            topic,
            generation,
            shared,
            requests,
            waiters: BarrierWaiters::process(),
            task,
        })
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn readiness(&self) -> Readiness {
        *self.shared.readiness.borrow()
    }

    pub fn state(&self) -> Presences {
        self.shared
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .presences
            .clone()
    }

    pub fn revision(&self) -> Revision {
        *self.shared.revision.borrow()
    }

    pub fn generation(&self) -> StreamGeneration {
        self.generation
    }

    pub fn cursor(&self) -> ViewCursor {
        self.shared.state.read().unwrap_or_else(PoisonError::into_inner).cursor
    }

    pub async fn read(&self, barrier: &ReadBarrier) -> Result<ViewSnapshot, BarrierError> {
        barrier.admit(self.generation, &self.topic)?;
        let budget = barrier.wait_budget(UnixMillis::now())?;
        let _permit = self.waiters.acquire(&self.topic)?;
        let (reply, answer) = oneshot::channel();
        let request = BarrierRequest {
            barrier: barrier.clone(),
            reply,
        };
        if self.requests.send(request).await.is_err() {
            return Err(BarrierError::NotReady);
        }
        match tokio::time::timeout(budget, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BarrierError::NotReady),
            Err(_) if self.readiness() == Readiness::Rebuilding => Err(BarrierError::NotReady),
            Err(_) => Err(barrier.expired()),
        }
    }

    pub fn get_by_key(&self, key: &PresenceKey) -> Vec<MetaEntry> {
        self.shared
            .state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .presences
            .get(key)
            .map(<[MetaEntry]>::to_vec)
            .unwrap_or_default()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ViewDiff> {
        self.shared.diffs.subscribe()
    }

    pub fn sync_state(&self) -> (ViewSnapshot, broadcast::Receiver<ViewDiff>) {
        let state = self.shared.state.read().unwrap_or_else(PoisonError::into_inner);
        let snapshot = ViewSnapshot {
            presences: state.presences.clone(),
            cursor: state.cursor,
        };
        (snapshot, self.shared.diffs.subscribe())
    }

    pub fn counters(&self) -> WatchCounters {
        WatchCounters {
            rejected: self.shared.rejected.load(Ordering::Relaxed),
            swept: self.shared.swept.load(Ordering::Relaxed),
            suspended: self.shared.suspended.load(Ordering::Relaxed),
            reconnects: self.shared.reconnects.load(Ordering::Relaxed),
            reconciles: self.shared.reconciles.load(Ordering::Relaxed),
        }
    }
}

impl Shared {
    fn publish(&self, snapshot: ViewSnapshot, revision: Option<Revision>, diff: Option<ViewDiff>) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.presences = snapshot.presences;
        state.cursor = snapshot.cursor;
        if let Some(diff) = diff {
            let _ = self.diffs.send(diff);
        }
        drop(state);
        self.advance(revision);
    }

    fn advance(&self, revision: Option<Revision>) {
        let Some(revision) = revision else {
            return;
        };
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.revision = state.revision.max(revision);
        drop(state);
        self.revision.send_if_modified(|seen| {
            let advanced = revision > *seen;
            if advanced {
                *seen = revision;
            }
            advanced
        });
    }

    fn mark(&self, readiness: Readiness) {
        self.readiness.send_if_modified(|current| {
            let changed = *current != readiness;
            *current = readiness;
            changed
        });
    }
}

impl Drop for TopicWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Source {
    stream: Stream,
    classifier: Classifier,
    budget: RebuildBudget,
}

impl Source {
    async fn rebuild(&self, shared: &Shared) -> Result<(LiveSet, ReplayConsumer), ReplayError> {
        let mut live = LiveSet::default();
        let replay = ReplayConsumer::rebuild(&self.stream, &self.classifier.filters(), self.budget, |record| {
            live.advance(record.stream_seq());
            if let Some(observation) = self.observe(&record, shared) {
                live.apply(observation);
            }
        })
        .await?;
        Ok((live, replay))
    }

    fn observe(&self, record: &RawRecord, shared: &Shared) -> Option<Observation> {
        match self.classifier.classify(record) {
            Classified::Observed(observation) => Some(observation),
            Classified::Rejected { reason, evict } => {
                shared.rejected.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(%reason, revision = record.stream_seq().get(), "dropped presence entry");
                evict
            }
        }
    }
}

struct Runner<F> {
    topic: Topic,
    source: Source,
    core: WatchCore,
    shared: Arc<Shared>,
    fetcher: F,
    window: CoalesceWindow,
    reconcile: ReconcileInterval,
    stale: StalePolicy,
    retirement: RetirementWindow,
    view: LocalViewId,
    incarnation: ViewIncarnation,
    cursor: ViewCursor,
    inbox: mpsc::Receiver<BarrierRequest>,
    pending: Vec<BarrierRequest>,
    audit: ReconnectAudit,
}

impl<F: MetaFetcher> Runner<F> {
    async fn run(mut self, mut replay: ReplayConsumer) {
        let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let period = self.reconcile.get();
        let mut reconcile = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut deadline: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                next = replay.next() => match next {
                    Ok(record) => {
                        self.receive(&record);
                        self.settle().await;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "presence watch replay failed, rebuilding");
                        replay = self.recover(replay).await;
                        deadline = None;
                        self.settle().await;
                    }
                },
                Some(request) = self.inbox.recv() => {
                    self.pending.push(request);
                    self.settle().await;
                }
                () = wait_until(deadline) => {
                    deadline = None;
                    self.flush().await;
                }
                _ = sweep.tick() => {
                    self.sweep(&replay).await;
                    if let AuditVerdict::Stranded { delivered, applied } = self.audit.check(&replay).await {
                        let err = ReplayError::Stranded { applied, delivered };
                        tracing::warn!(%err, "presence watch lost deliveries across a reconnect, rebuilding");
                        replay = self.recover(replay).await;
                        deadline = None;
                        self.settle().await;
                    }
                }
                _ = reconcile.tick() => {
                    replay = self.reconcile(replay).await;
                    deadline = None;
                    self.settle().await;
                }
            }
            if !self.core.has_pending() {
                self.shared.advance(self.core.revision());
            } else if deadline.is_none() {
                deadline = Some(tokio::time::Instant::now() + self.window.get());
            }
        }
    }

    fn receive(&mut self, record: &RawRecord) {
        self.core.advance(record.stream_seq());
        if let Some(observation) = self.source.observe(record, &self.shared) {
            self.core.apply(observation);
        }
    }

    async fn settle(&mut self) {
        self.pending.retain(|request| !request.reply.is_closed());
        if self.pending.is_empty() {
            return;
        }
        let mut answered = Vec::new();
        let mut waiting = Vec::new();
        for request in std::mem::take(&mut self.pending) {
            match request.barrier.verdict(&self.core) {
                Verdict::Pending => waiting.push(request),
                verdict => answered.push((request, verdict)),
            }
        }
        self.pending = waiting;
        if answered.is_empty() {
            return;
        }
        self.flush().await;
        let rendered = !self.core.has_pending();
        let snapshot = self.snapshot();
        for (request, verdict) in answered {
            let answer = match verdict {
                Verdict::Satisfied if rendered => Ok(snapshot.clone()),
                _ => Err(request.barrier.unavailable()),
            };
            let _ = request.reply.send(answer);
        }
    }

    async fn sweep(&mut self, replay: &ReplayConsumer) {
        let now = SystemTime::now();
        self.core.forget_retired(now, self.retirement);
        if !self.core.has_stale(now, self.stale) {
            return;
        }
        match self.core.sweep(replay.watermark().await, now, self.stale) {
            SweepOutcome::Suspended(watermark) => {
                self.shared.suspended.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(?watermark, "presence sweep suspended until the replay catches up");
            }
            SweepOutcome::Swept(0) => {}
            SweepOutcome::Swept(swept) => {
                self.shared.swept.fetch_add(swept as u64, Ordering::Relaxed);
                tracing::warn!(swept, "swept presence entries that never received an expiry marker");
            }
        }
    }

    async fn recover(&mut self, failed: ReplayConsumer) -> ReplayConsumer {
        self.audit.forget();
        self.shared.mark(Readiness::Rebuilding);
        failed.discard();
        let mut backoff = RetryBackoff::default();
        loop {
            match self.source.rebuild(&self.shared).await {
                Ok((fresh, replay)) => {
                    self.shared.reconnects.fetch_add(1, Ordering::Relaxed);
                    self.swap(fresh).await;
                    self.reincarnate();
                    self.shared.mark(Readiness::Ready);
                    return replay;
                }
                Err(err) => {
                    let delay = backoff.jittered();
                    tracing::warn!(%err, ?delay, "presence watch rebuild failed");
                    tokio::time::sleep(delay).await;
                    backoff = backoff.next();
                }
            }
        }
    }

    async fn reconcile(&mut self, current: ReplayConsumer) -> ReplayConsumer {
        match self.source.rebuild(&self.shared).await {
            Ok((fresh, replay)) => {
                current.discard();
                self.audit.forget();
                self.shared.reconciles.fetch_add(1, Ordering::Relaxed);
                self.swap(fresh).await;
                replay
            }
            Err(err) => {
                tracing::warn!(%err, "presence watch reconcile failed, keeping the current view");
                current
            }
        }
    }

    async fn swap(&mut self, fresh: LiveSet) {
        self.core.reconcile(fresh);
        self.flush().await;
    }

    fn reincarnate(&mut self) {
        let prev = self.cursor.seq();
        match self.incarnation.next() {
            Ok(incarnation) => self.incarnation = incarnation,
            Err(err) => tracing::error!(%err, "presence view incarnation overflowed, keeping the last one"),
        }
        self.cursor = ViewCursor::local(self.view, self.incarnation);
        self.publish(Some((prev, Diff::default())));
    }

    async fn render(&mut self) -> Option<Diff> {
        let batch = self.core.render();
        if batch.is_empty() {
            return None;
        }
        let fetched = match self.fetcher.fetch(&self.topic, batch.requests()).await {
            Ok(fetched) => fetched,
            Err(err) => {
                tracing::warn!(%err, "presence meta fetch failed, keeping the previous view");
                self.core.restore(batch);
                return None;
            }
        };
        match self.core.commit(batch, fetched) {
            Ok(diff) => Some(diff),
            Err((batch, err)) => {
                tracing::warn!(%err, "presence meta fetch failed, keeping the previous view");
                self.core.restore(batch);
                None
            }
        }
    }

    async fn flush(&mut self) {
        let Some(diff) = self.render().await else {
            return;
        };
        if diff.is_empty() {
            return;
        }
        let prev = self.cursor.seq();
        self.cursor = match self.cursor.advance() {
            Ok(cursor) => cursor,
            Err(_) => {
                self.reincarnate();
                return;
            }
        };
        self.publish(Some((prev, diff)));
    }

    fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            presences: self.core.presences(),
            cursor: self.cursor,
        }
    }

    fn publish(&self, diff: Option<(DiffSequence, Diff)>) {
        let diff = diff.map(|(prev, diff)| ViewDiff {
            cursor: self.cursor,
            prev,
            diff,
        });
        self.shared.publish(self.snapshot(), self.core.revision(), diff);
    }
}

async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
