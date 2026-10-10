use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_nats::{HeaderMap, Message, Subject};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use trogon_presence::watch::replay::RetryBackoff;
use trogon_presence::watch::{CursorStep, ViewCursor};
use trogon_presence::{
    ConnectionId, Diff, DiffSequence, EntropyError, EntryRevision, EpochOrder, GenerationEpoch, LocalViewId, MetaEntry,
    OwnerEpoch, OwnerId, PresenceKey, Presences, RequestId, ShardCount, StreamGeneration, Topic, ViewShard,
};

use crate::inbox::CALLER_INBOX_PREFIX;
use crate::reply::{
    FrameKind, ReplyCode, HEADER_CODE, HEADER_GENERATION, HEADER_KIND, HEADER_OWNER_ID, HEADER_OWNER_REV, HEADER_PREV,
    HEADER_SEQ,
};
use crate::snapshot::{
    Assembly, AssemblyError, AssemblyGate, AssemblyPermit, AssemblyProgress, SnapshotFrame, SnapshotLimits,
    SnapshotManifest,
};
use crate::subjects::{diff_subject, epoch_subject, snapshot_subject, SnapshotReplySubject};

const DEFAULT_RESNAPSHOT_SECS: u64 = 30;
const RECONNECT_POLL: Duration = Duration::from_millis(250);
const EVENT_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResnapshotInterval(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resnapshot interval must be greater than zero")]
pub struct ResnapshotIntervalError;

impl ResnapshotInterval {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for ResnapshotInterval {
    fn default() -> Self {
        Self(Duration::from_secs(DEFAULT_RESNAPSHOT_SECS))
    }
}

impl TryFrom<Duration> for ResnapshotInterval {
    type Error = ResnapshotIntervalError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(ResnapshotIntervalError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderIdentity {
    key: PresenceKey,
    connection: ConnectionId,
}

impl ReaderIdentity {
    pub fn new(key: PresenceKey, connection: ConnectionId) -> Self {
        Self { key, connection }
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn connection(&self) -> &ConnectionId {
        &self.connection
    }
}

#[derive(Debug, Clone)]
pub struct ReaderOptions {
    shards: ShardCount,
    limits: SnapshotLimits,
    resnapshot: ResnapshotInterval,
    gate: AssemblyGate,
}

impl Default for ReaderOptions {
    fn default() -> Self {
        let limits = SnapshotLimits::default();
        Self {
            shards: ShardCount::DEFAULT,
            limits,
            resnapshot: ResnapshotInterval::default(),
            gate: AssemblyGate::new(limits),
        }
    }
}

impl ReaderOptions {
    pub fn with_shards(self, shards: ShardCount) -> Self {
        Self { shards, ..self }
    }

    pub fn with_limits(self, limits: SnapshotLimits) -> Self {
        Self {
            limits,
            gate: AssemblyGate::new(limits),
            ..self
        }
    }

    pub fn with_resnapshot(self, resnapshot: ResnapshotInterval) -> Self {
        Self { resnapshot, ..self }
    }

    pub fn with_gate(self, gate: AssemblyGate) -> Self {
        Self { gate, ..self }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppliedDiff {
    cursor: ViewCursor,
    diff: Diff,
}

impl AppliedDiff {
    pub fn cursor(&self) -> ViewCursor {
        self.cursor
    }

    pub fn diff(&self) -> &Diff {
        &self.diff
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReaderEvent {
    Snapshot { cursor: ViewCursor, state: Presences },
    Diff(AppliedDiff),
}

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error(transparent)]
    Subscribe(#[from] async_nats::SubscribeError),
    #[error(transparent)]
    Flush(#[from] async_nats::client::FlushError),
    #[error(transparent)]
    Entropy(#[from] EntropyError),
}

pub struct PresenceReader {
    state: watch::Receiver<Presences>,
    events: broadcast::Sender<ReaderEvent>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl PresenceReader {
    pub async fn start(
        client: async_nats::Client,
        identity: ReaderIdentity,
        topic: Topic,
        options: ReaderOptions,
    ) -> Result<Self, ReaderError> {
        let view = LocalViewId::generate()?;
        let inbox_prefix = format!(
            "{CALLER_INBOX_PREFIX}.{}.{}.{view}",
            identity.key.token(),
            identity.connection
        );
        let shard = ViewShard::of(&topic, options.shards);
        let diffs = client.subscribe(diff_subject(&topic)).await?;
        let parts = client
            .subscribe(SnapshotReplySubject::filter(&identity.key, &identity.connection))
            .await?;
        let replies = client.subscribe(format!("{inbox_prefix}.*")).await?;
        let epochs = client.subscribe(epoch_subject(options.shards, shard)).await?;
        client.flush().await?;
        let (state_tx, state) = watch::channel(Presences::default());
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (stop, stop_rx) = watch::channel(false);
        let machine = Machine {
            client,
            request_subject: snapshot_subject(options.shards, &identity.key, &identity.connection, &topic),
            identity,
            inbox_prefix,
            options,
            installed: None,
            pending: None,
            retry_at: Some(Instant::now()),
            backoff: RetryBackoff::default(),
            next_resnapshot: None,
            state: state_tx,
            events: events.clone(),
        };
        let task = tokio::spawn(machine.run(
            Subscriptions {
                diffs,
                parts,
                replies,
                epochs,
            },
            stop_rx,
        ));
        Ok(Self {
            state,
            events,
            stop,
            task,
        })
    }

    pub fn presences(&self) -> Presences {
        self.state.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<Presences> {
        self.state.clone()
    }

    pub fn events(&self) -> broadcast::Receiver<ReaderEvent> {
        self.events.subscribe()
    }

    pub async fn close(self) {
        let _ = self.stop.send(true);
        if let Err(err) = self.task.await {
            tracing::warn!(%err, "presence reader task failed");
        }
    }
}

struct Subscriptions {
    diffs: async_nats::Subscriber,
    parts: async_nats::Subscriber,
    replies: async_nats::Subscriber,
    epochs: async_nats::Subscriber,
}

struct Installed {
    cursor: ViewCursor,
    state: BTreeMap<PresenceKey, Vec<MetaEntry>>,
}

struct BufferedFrame {
    cursor: ViewCursor,
    prev: DiffSequence,
    diff: Option<Diff>,
}

struct Pending {
    request: RequestId,
    deadline: Instant,
    assembly: Option<(Assembly, AssemblyPermit)>,
    stash: Vec<SnapshotFrame>,
    stash_bytes: usize,
    buffer: VecDeque<BufferedFrame>,
    buffer_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
enum Abandon {
    #[error("snapshot request refused: {0}")]
    Refused(String),
    #[error(transparent)]
    Assembly(AssemblyError),
    #[error(transparent)]
    Gate(crate::snapshot::AssemblyRefusal),
    #[error("snapshot assembly buffer overflowed")]
    Overflow,
    #[error("snapshot assembly deadline elapsed")]
    Deadline,
}

#[derive(Debug, Deserialize)]
struct EpochHint {
    generation: StreamGeneration,
    owner_epoch: OwnerEpoch,
}

struct Machine {
    client: async_nats::Client,
    request_subject: String,
    identity: ReaderIdentity,
    inbox_prefix: String,
    options: ReaderOptions,
    installed: Option<Installed>,
    pending: Option<Pending>,
    retry_at: Option<Instant>,
    backoff: RetryBackoff,
    next_resnapshot: Option<Instant>,
    state: watch::Sender<Presences>,
    events: broadcast::Sender<ReaderEvent>,
}

impl Machine {
    async fn run(mut self, mut subs: Subscriptions, mut stop: watch::Receiver<bool>) {
        let stats = self.client.statistics();
        let mut connects = stats.connects.load(Ordering::Relaxed);
        let mut poll = tokio::time::interval(RECONNECT_POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let deadline = self.pending.as_ref().map(|pending| pending.deadline);
            tokio::select! {
                _ = stop.changed() => break,
                Some(message) = subs.diffs.next() => self.on_diff(&message),
                Some(message) = subs.parts.next() => self.on_part(&message),
                Some(message) = subs.replies.next() => self.on_reply(&message),
                Some(message) = subs.epochs.next() => self.on_epoch(&message),
                () = wait_until(deadline) => self.abandon(Abandon::Deadline),
                () = wait_until(self.retry_at) => {
                    self.retry_at = None;
                    self.request().await;
                }
                () = wait_until(self.next_resnapshot) => {
                    self.next_resnapshot = None;
                    self.resnapshot();
                }
                _ = poll.tick() => {
                    let now = stats.connects.load(Ordering::Relaxed);
                    if now != connects {
                        connects = now;
                        self.resnapshot();
                    }
                }
            }
        }
        let _ = subs.diffs.unsubscribe().await;
        let _ = subs.parts.unsubscribe().await;
        let _ = subs.replies.unsubscribe().await;
        let _ = subs.epochs.unsubscribe().await;
    }

    fn resnapshot(&mut self) {
        if self.pending.is_none() && self.retry_at.is_none() {
            self.retry_at = Some(Instant::now());
        }
    }

    async fn request(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let request = match RequestId::generate() {
            Ok(request) => request,
            Err(err) => {
                tracing::warn!(%err, "could not mint a snapshot request id");
                return self.schedule_retry();
            }
        };
        let body = match serde_json::to_vec(&serde_json::json!({ "request_id": request })) {
            Ok(body) => body,
            Err(err) => {
                tracing::warn!(%err, "could not encode a snapshot request");
                return self.schedule_retry();
            }
        };
        self.pending = Some(Pending {
            request,
            deadline: Instant::now() + self.options.limits.deadline().get(),
            assembly: None,
            stash: Vec::new(),
            stash_bytes: 0,
            buffer: VecDeque::new(),
            buffer_bytes: 0,
        });
        let inbox = Subject::from(format!("{}.{request}", self.inbox_prefix));
        if let Err(err) = self
            .client
            .publish_with_reply(self.request_subject.clone(), inbox, body.into())
            .await
        {
            tracing::warn!(%err, "could not send a snapshot request");
            self.abandon(Abandon::Refused(err.to_string()));
        }
    }

    fn schedule_retry(&mut self) {
        self.retry_at = Some(Instant::now() + self.backoff.jittered());
        self.backoff = self.backoff.next();
    }

    fn abandon(&mut self, reason: Abandon) {
        tracing::debug!(%reason, "abandoning a snapshot assembly");
        self.pending = None;
        self.schedule_retry();
    }

    fn on_reply(&mut self, message: &Message) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let addressed = message
            .subject
            .as_str()
            .rsplit_once('.')
            .and_then(|(_, token)| token.parse::<RequestId>().ok());
        if addressed != Some(pending.request) {
            return;
        }
        let code = header(message.headers.as_ref(), HEADER_CODE);
        if code != Some(ReplyCode::Ok.as_str()) {
            let detail = String::from_utf8_lossy(&message.payload).into_owned();
            return self.abandon(Abandon::Refused(detail));
        }
        let manifest: SnapshotManifest = match serde_json::from_slice(&message.payload) {
            Ok(manifest) => manifest,
            Err(err) => return self.abandon(Abandon::Refused(err.to_string())),
        };
        if manifest.identity().request() != pending.request || pending.assembly.is_some() {
            return self.abandon(Abandon::Refused("manifest does not match the request".to_owned()));
        }
        let assembly = match Assembly::begin(manifest, self.options.limits) {
            Ok(assembly) => assembly,
            Err(err) => return self.abandon(Abandon::Assembly(err)),
        };
        let permit = match self
            .options
            .gate
            .admit(&self.identity.connection, manifest.total_bytes())
        {
            Ok(permit) => permit,
            Err(refusal) => return self.abandon(Abandon::Gate(refusal)),
        };
        pending.assembly = Some((assembly, permit));
        pending.stash_bytes = 0;
        for frame in std::mem::take(&mut pending.stash) {
            if self.feed(frame) {
                return;
            }
        }
    }

    fn on_part(&mut self, message: &Message) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let frame = match SnapshotFrame::try_from(message) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::debug!(%err, "ignoring a malformed snapshot frame");
                return;
            }
        };
        if frame.identity().request() != pending.request {
            return;
        }
        if pending.assembly.is_some() {
            self.feed(frame);
            return;
        }
        pending.stash_bytes = pending.stash_bytes.saturating_add(frame.payload().len());
        let max_frames = usize::try_from(self.options.limits.max_parts().get()).unwrap_or(usize::MAX);
        if pending.stash_bytes > self.options.limits.max_bytes().get() || pending.stash.len() > max_frames {
            return self.abandon(Abandon::Overflow);
        }
        pending.stash.push(frame);
    }

    fn feed(&mut self, frame: SnapshotFrame) -> bool {
        let Some((assembly, _)) = self.pending.as_mut().and_then(|pending| pending.assembly.as_mut()) else {
            return true;
        };
        let cursor = assembly.manifest().identity().cursor();
        match assembly.accept(frame) {
            Ok(AssemblyProgress::Pending) => false,
            Ok(AssemblyProgress::Complete(state)) => {
                self.install(cursor, state);
                true
            }
            Err(err) => {
                self.abandon(Abandon::Assembly(err));
                true
            }
        }
    }

    fn install(&mut self, cursor: ViewCursor, state: Presences) {
        let buffered = self.pending.take().map(|pending| pending.buffer).unwrap_or_default();
        self.backoff = RetryBackoff::default();
        self.next_resnapshot = Some(Instant::now() + self.options.resnapshot.get());
        self.installed = Some(Installed {
            cursor,
            state: state.clone().into_map(),
        });
        let _ = self.events.send(ReaderEvent::Snapshot { cursor, state });
        self.publish_state();
        for frame in buffered {
            if !self.apply(frame) {
                break;
            }
        }
    }

    fn on_diff(&mut self, message: &Message) {
        let Some(frame) = parse_diff(message) else {
            tracing::debug!("diff frame is malformed, resnapshotting");
            return self.resnapshot();
        };
        if let Some(pending) = self.pending.as_mut() {
            pending.buffer_bytes = pending.buffer_bytes.saturating_add(message.payload.len());
            if pending.buffer_bytes > self.options.limits.diff_buffer().get() {
                return self.abandon(Abandon::Overflow);
            }
            pending.buffer.push_back(frame);
            return;
        }
        self.apply(frame);
    }

    fn apply(&mut self, frame: BufferedFrame) -> bool {
        let Some(installed) = self.installed.as_mut() else {
            return false;
        };
        match installed.cursor.follow(&frame.cursor, frame.prev) {
            CursorStep::Repeat | CursorStep::Stale => true,
            CursorStep::Gap | CursorStep::Rebase => {
                self.resnapshot();
                false
            }
            CursorStep::Next => {
                installed.cursor = frame.cursor;
                if let Some(diff) = frame.diff {
                    sync_diff(&mut installed.state, &diff);
                    let applied = AppliedDiff {
                        cursor: frame.cursor,
                        diff,
                    };
                    let _ = self.events.send(ReaderEvent::Diff(applied));
                    self.publish_state();
                }
                true
            }
        }
    }

    fn on_epoch(&mut self, message: &Message) {
        let Ok(hint) = serde_json::from_slice::<EpochHint>(&message.payload) else {
            return self.resnapshot();
        };
        let hinted = GenerationEpoch::new(hint.generation, hint.owner_epoch);
        let current = self.installed.as_ref().and_then(|installed| match installed.cursor {
            ViewCursor::Service { epoch, .. } => Some(epoch),
            ViewCursor::Local { .. } => None,
        });
        let stale = match current.map(|current| current.compare(hinted)) {
            Some(Ok(EpochOrder::Same | EpochOrder::Newer)) => true,
            Some(Ok(EpochOrder::Older | EpochOrder::Conflict) | Err(_)) | None => false,
        };
        if !stale {
            self.resnapshot();
        }
    }

    fn publish_state(&self) {
        if let Some(installed) = &self.installed {
            let _ = self.state.send(Presences::from(installed.state.clone()));
        }
    }
}

fn header<'a>(headers: Option<&'a HeaderMap>, name: &str) -> Option<&'a str> {
    headers
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn parse_diff(message: &Message) -> Option<BufferedFrame> {
    let headers = message.headers.as_ref();
    let generation: StreamGeneration = header(headers, HEADER_GENERATION)?.parse().ok()?;
    let owner: OwnerId = header(headers, HEADER_OWNER_ID)?.parse().ok()?;
    let acquired: u64 = header(headers, HEADER_OWNER_REV)?.parse().ok()?;
    let seq: u64 = header(headers, HEADER_SEQ)?.parse().ok()?;
    let prev: u64 = header(headers, HEADER_PREV)?.parse().ok()?;
    let epoch = GenerationEpoch::new(generation, OwnerEpoch::new(EntryRevision::from(acquired), owner));
    let cursor = ViewCursor::Service {
        epoch,
        seq: DiffSequence::from(seq),
    };
    let kind = header(headers, HEADER_KIND)?;
    let diff = if kind == FrameKind::Diff.as_str() {
        Some(serde_json::from_slice::<Diff>(&message.payload).ok()?)
    } else if kind == FrameKind::Keepalive.as_str() {
        None
    } else {
        return None;
    };
    Some(BufferedFrame {
        cursor,
        prev: DiffSequence::from(prev),
        diff,
    })
}

pub fn sync_diff(state: &mut BTreeMap<PresenceKey, Vec<MetaEntry>>, diff: &Diff) {
    for (key, joins) in diff.joins().iter() {
        let joined: Vec<&trogon_presence::ViewRef> = joins.iter().map(MetaEntry::phx_ref).collect();
        let mut metas: Vec<MetaEntry> = state
            .remove(key)
            .unwrap_or_default()
            .into_iter()
            .filter(|meta| !joined.contains(&meta.phx_ref()))
            .collect();
        metas.extend(joins.iter().cloned());
        state.insert(key.clone(), metas);
    }
    for (key, leaves) in diff.leaves().iter() {
        let Some(current) = state.get_mut(key) else {
            continue;
        };
        let left: Vec<&trogon_presence::ViewRef> = leaves.iter().map(MetaEntry::phx_ref).collect();
        current.retain(|meta| !left.contains(&meta.phx_ref()));
        if current.is_empty() {
            state.remove(key);
        }
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
