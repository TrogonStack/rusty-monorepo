#[allow(dead_code)]
mod common;
#[allow(dead_code)]
#[path = "common/cluster.rs"]
mod r3;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

use trogon_presence::{
    CursorStep, EntryKey, HeartbeatInterval, HolderId, LeaseTtl, MarkerTtl, Presence, PresenceConfig, PresenceKey,
    ProvisionOptions, Readiness, Replicas, RetryWindow, ShardCount, Topic, WriterMode,
};
use trogon_presence_service::subjects::HolderOp;
use trogon_presence_service::{
    KeepaliveInterval, NodeId, RequestIdentity, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
    CALLER_INBOX_PREFIX,
};

use common::BoxError;
use r3::{Cluster, Peer, Wait};

type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const SHARD_LEASE_TTL: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_PAUSE: Duration = Duration::from_millis(100);
const SAMPLE_EVERY: Duration = Duration::from_millis(10);
const OWNERSHIP: Wait = Wait::new("every shard to be owned", Duration::from_secs(60));
const WRITES_RECOVER: Wait = Wait::new("writes to recover", Duration::from_secs(60));
const NEW_LEADER: Wait = Wait::new("a new stream leader", Duration::from_secs(60));
const REPLICAS_CURRENT: Wait = Wait::new("every replica to be current", Duration::from_secs(60));
const QUORUM_REFUSAL: Wait = Wait::new("a typed refusal without quorum", Duration::from_secs(60));
const WATCH_CATCHES_UP: Wait = Wait::new("the watcher to see every write", Duration::from_secs(60));
const EXPIRY_MARGIN: Duration = Duration::from_secs(5);
const UNAVAILABLE: [&str; 3] = ["unavailable", "not_ready", "not_owner"];

macro_rules! cluster_or_skip {
    () => {
        match Cluster::start().await {
            Some(cluster) => cluster,
            None => return Ok(()),
        }
    };
}

fn presence_config() -> Result<PresenceConfig, BoxError> {
    Ok(PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(Duration::from_secs(60))?,
        HeartbeatInterval::try_from(Duration::from_secs(10))?,
        MarkerTtl::try_from(Duration::from_secs(120))?,
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed))
}

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    Ok(ServiceConfig::new(presence_config()?, node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(SHARD_LEASE_TTL)?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

fn presence_stream() -> Result<String, BoxError> {
    Ok(presence_config()?.bucket().stream_name())
}

fn shard_count() -> usize {
    usize::from(ShardCount::DEFAULT.get())
}

async fn provision(client: &async_nats::Client) -> TestResult {
    let options = ProvisionOptions {
        storage: StorageType::File,
        replicas: Replicas::try_from(3)?,
    };
    trogon_presence_service::provision(client.clone(), &service_config("provisioner")?, options).await?;
    Ok(())
}

async fn start(client: async_nats::Client, node: &str) -> Result<ServiceHandle, BoxError> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    Ok(trogon_presence_service::start(client, service_config(node)?).await?)
}

async fn owning_all(handle: &ServiceHandle) -> TestResult {
    OWNERSHIP
        .until(|| async {
            (handle.writer_shards().len() == shard_count() && handle.owned_shards().len() == shard_count())
                .then_some(())
        })
        .await
}

/// Waits until the presence and shard lease streams each report a leader with a current replica, and every
/// shard is owned again, so that writes issued afterwards start their retry window in steady state.
async fn steady(client: &async_nats::Client, handle: &ServiceHandle) -> TestResult {
    let ready = Instant::now();
    Cluster::wait_for_quorum(client, &presence_stream()?, REPLICAS_CURRENT).await?;
    let lease_stream = service_config("probe")?.lease_bucket().stream_name();
    Cluster::wait_for_quorum(client, &lease_stream, REPLICAS_CURRENT).await?;
    Cluster::wait_for_consumers(client, &presence_stream()?, REPLICAS_CURRENT).await?;
    owning_all(handle).await?;
    eprintln!("steady state reached {:?} after the first landing", ready.elapsed());
    Ok(())
}

#[derive(Debug, Clone)]
struct Intent {
    key: PresenceKey,
    topic: Topic,
    holder: HolderId,
    request: Value,
}

impl Intent {
    fn new(key: &str) -> Result<Self, BoxError> {
        let window = RetryWindow::for_ttl(presence_config()?.lease_ttl());
        Ok(Self {
            key: key.parse()?,
            topic: TOPIC.parse()?,
            holder: HolderId::generate()?,
            request: serde_json::to_value(RequestIdentity::mint(window)?)?,
        })
    }

    fn body(&self) -> Value {
        json!({ "holder": self.holder, "meta": { "status": "online" }, "request": self.request })
    }

    fn beat(&self, receipt: &Value) -> Value {
        json!({ "entries": [{
            "holder": self.holder,
            "topic": self.topic,
            "lifetime": receipt["lifetime"],
            "mutation_seq": receipt["mutation_seq"],
        }] })
    }
}

#[derive(Debug)]
enum Reply {
    Coded(String, Value),
    Silent,
}

impl Reply {
    fn code(&self) -> &str {
        match self {
            Self::Coded(code, _) => code,
            Self::Silent => "no reply",
        }
    }
}

/// Run-length summary of the replies one logical write saw, as `code xcount until elapsed`.
#[derive(Default)]
struct Trace {
    runs: Vec<(String, usize, Duration)>,
}

impl Trace {
    fn record(&mut self, started: Instant, reply: &Reply) {
        let at = started.elapsed();
        let seen = match reply {
            Reply::Coded(code, body) => match body["detail"].as_str() {
                Some(detail) => format!("{code} ({detail})"),
                None => code.clone(),
            },
            Reply::Silent => reply.code().to_owned(),
        };
        match self.runs.last_mut() {
            Some((last, count, until)) if *last == seen => {
                *count += 1;
                *until = at;
            }
            _ => self.runs.push((seen, 1, at)),
        }
    }
}

impl std::fmt::Display for Trace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (code, count, until) in &self.runs {
            write!(formatter, "[{code} x{count} until {until:.2?}]")?;
        }
        Ok(())
    }
}

struct Writer {
    client: async_nats::Client,
}

impl Writer {
    async fn send(&self, subject: String, key: &PresenceKey, body: &Value) -> Result<Reply, BoxError> {
        let inbox = format!("{CALLER_INBOX_PREFIX}.{}.c1.{}", key.token(), HolderId::generate()?);
        let mut replies = self.client.subscribe(inbox.clone()).await?;
        self.client
            .publish_with_reply(subject, inbox, serde_json::to_vec(body)?.into())
            .await?;
        self.client.flush().await?;
        Ok(match tokio::time::timeout(REPLY_TIMEOUT, replies.next()).await {
            Ok(Some(message)) => {
                let code = common::code_of(&message).ok_or("reply has no code")?.to_owned();
                Reply::Coded(code, common::body_of(&message)?)
            }
            Ok(None) | Err(_) => Reply::Silent,
        })
    }

    async fn track(&self, intent: &Intent) -> Result<Reply, BoxError> {
        self.send(
            WriteOp::Track.subject(&intent.key, &intent.topic),
            &intent.key,
            &intent.body(),
        )
        .await
    }

    async fn heartbeat(&self, intent: &Intent, receipt: &Value) -> Result<Reply, BoxError> {
        self.send(
            HolderOp::Heartbeat.subject(&intent.key),
            &intent.key,
            &intent.beat(receipt),
        )
        .await
    }

    /// Lands a freshly minted write, then sends the same request again inside its retry window and checks
    /// that the retry replays the original receipt and leaves the stream untouched.
    async fn land_once(&self, key: &str) -> Result<(Intent, Value), BoxError> {
        let intent = Intent::new(key)?;
        let receipt = self.land(&intent, WRITES_RECOVER).await?;
        let stored = stored_revision(&self.client, &intent).await?;
        assert_eq!(
            revision_of(&receipt)?,
            stored,
            "{key} receipt does not match the stream"
        );
        let replayed = self.replay(&intent).await?;
        assert_eq!(replayed["rev"], receipt["rev"], "{key} retry produced a second write");
        assert_eq!(
            stored_revision(&self.client, &intent).await?,
            stored,
            "{key} retry wrote again"
        );
        Ok((intent, receipt))
    }

    /// Sends a request that already landed again with the same identity and returns the replayed receipt.
    async fn replay(&self, intent: &Intent) -> Result<Value, BoxError> {
        let started = Instant::now();
        let mut trace = Trace::default();
        loop {
            let reply = self.track(intent).await?;
            trace.record(started, &reply);
            match reply {
                Reply::Coded(code, body) if code == "ok" => return Ok(body),
                Reply::Coded(code, body) if !UNAVAILABLE.contains(&code.as_str()) => {
                    return Err(format!("{} retry replied {code}: {body}; attempts {trace}", intent.key).into());
                }
                _ => {}
            }
            if started.elapsed() >= WRITES_RECOVER.within {
                return Err(format!("timed out replaying {}; attempts {trace}", intent.key).into());
            }
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    }

    /// Retries one logical write, with one request identity, until it lands.
    async fn land(&self, intent: &Intent, wait: Wait) -> Result<Value, BoxError> {
        let started = Instant::now();
        let mut trace = Trace::default();
        loop {
            let reply = self.track(intent).await?;
            trace.record(started, &reply);
            match reply {
                Reply::Coded(code, body) if code == "ok" => return Ok(body),
                Reply::Coded(code, body) if !UNAVAILABLE.contains(&code.as_str()) => {
                    return Err(format!("{} replied {code}: {body}; attempts {trace}", intent.key).into());
                }
                _ => {}
            }
            if started.elapsed() >= wait.within {
                return Err(format!(
                    "timed out after {:?} waiting for {}; attempts {trace}",
                    wait.within, wait.what
                )
                .into());
            }
            tokio::time::sleep(RETRY_PAUSE).await;
        }
    }

    /// Probes until a write lands, using a new key for every attempt so that an attempt whose outcome is
    /// unknown can never be written twice. Returns the time until the first landing.
    async fn first_landing(&self, prefix: &str, wait: Wait) -> Result<Duration, BoxError> {
        let started = Instant::now();
        for attempt in 0_u32.. {
            let intent = Intent::new(&format!("{prefix}-{attempt}"))?;
            match self.track(&intent).await? {
                Reply::Coded(code, _) if code == "ok" => return Ok(started.elapsed()),
                Reply::Coded(code, body) if !UNAVAILABLE.contains(&code.as_str()) => {
                    return Err(format!("{} replied {code}: {body}", intent.key).into());
                }
                _ => {}
            }
            if started.elapsed() >= wait.within {
                break;
            }
            tokio::time::sleep(RETRY_PAUSE).await;
        }
        Err(format!("timed out after {:?} waiting for {}", wait.within, wait.what).into())
    }
}

fn revision_of(receipt: &Value) -> Result<u64, BoxError> {
    Ok(receipt["rev"].as_str().ok_or("receipt has no rev")?.parse()?)
}

async fn stored_revision(client: &async_nats::Client, intent: &Intent) -> Result<u64, BoxError> {
    let config = presence_config()?;
    let kv_key = EntryKey::new(intent.topic.clone(), intent.key.clone(), intent.holder).encode(config.shards())?;
    let stream = jetstream::new(client.clone())
        .get_stream(config.bucket().stream_name())
        .await?;
    let message = stream
        .get_last_raw_message_by_subject(&config.bucket().subject_for(&kv_key))
        .await?;
    Ok(message.sequence)
}

/// Samples two services and counts every moment both claimed the same shard.
struct OverlapSampler {
    stop: watch::Sender<bool>,
    overlaps: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl OverlapSampler {
    fn spawn(first: Arc<ServiceHandle>, second: Arc<ServiceHandle>) -> Self {
        let (stop, mut stopped) = watch::channel(false);
        let overlaps = Arc::new(AtomicUsize::new(0));
        let counter = overlaps.clone();
        let task = tokio::spawn(async move {
            loop {
                let writers = second.writer_shards();
                let views = second.owned_shards();
                let doubled = first.writer_shards().iter().any(|shard| writers.contains(shard))
                    || first.owned_shards().iter().any(|shard| views.contains(shard));
                if doubled {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                tokio::select! {
                    _ = stopped.changed() => return,
                    () = tokio::time::sleep(SAMPLE_EVERY) => {}
                }
            }
        });
        Self { stop, overlaps, task }
    }

    async fn finish(self) -> Result<usize, BoxError> {
        let _ = self.stop.send(true);
        self.task.await?;
        Ok(self.overlaps.load(Ordering::SeqCst))
    }
}

/// Ownership churn one service saw while sampled: shards it dropped and later held again.
#[derive(Debug, Default, Clone, Copy)]
struct Churn {
    drops: usize,
    unowned: Duration,
}

impl std::fmt::Display for Churn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} shard drops, {:.2?} of shard time unowned",
            self.drops, self.unowned
        )
    }
}

struct ChurnSampler {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Churn>,
}

impl ChurnSampler {
    fn spawn(handle: Arc<ServiceHandle>) -> Self {
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut churn = Churn::default();
            let mut previous = handle.owned_shards();
            let mut sampled = Instant::now();
            loop {
                tokio::select! {
                    _ = stopped.changed() => return churn,
                    () = tokio::time::sleep(SAMPLE_EVERY) => {}
                }
                let owned = handle.owned_shards();
                let elapsed = sampled.elapsed();
                sampled = Instant::now();
                churn.drops += previous.iter().filter(|shard| !owned.contains(shard)).count();
                let missing = shard_count().saturating_sub(owned.len());
                churn.unowned += elapsed * u32::try_from(missing).unwrap_or(u32::MAX);
                previous = owned;
            }
        });
        Self { stop, task }
    }

    async fn finish(self) -> Result<Churn, BoxError> {
        let _ = self.stop.send(true);
        Ok(self.task.await?)
    }
}

async fn shutdown(handle: Arc<ServiceHandle>) -> TestResult {
    let handle = Arc::into_inner(handle).ok_or("service handle is still shared")?;
    tokio::time::timeout(Duration::from_secs(60), handle.shutdown())
        .await
        .map_err(|_| "service shutdown did not finish")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "three-node cluster fault test, run through `mise run presence:cluster`"]
async fn losing_the_owner_node_moves_ownership_after_the_fence_without_overlap() -> TestResult {
    let mut cluster = cluster_or_skip!();
    let probe = cluster.client(Peer::N3).await?;
    provision(&probe).await?;
    let first = Arc::new(start(cluster.client(Peer::N1).await?, "edge-a").await?);
    owning_all(&first).await?;
    let second = Arc::new(start(cluster.client(Peer::N2).await?, "edge-b").await?);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(second.writer_shards().is_empty(), "the standby took a writer shard");
    assert!(second.owned_shards().is_empty(), "the standby took a view shard");
    let writer = Writer { client: probe };
    writer.land(&Intent::new("before")?, WRITES_RECOVER).await?;

    let sampler = OverlapSampler::spawn(first.clone(), second.clone());
    cluster.stop(Peer::N1);
    let window = writer.first_landing("after", WRITES_RECOVER).await?;
    owning_all(&second).await?;
    let overlaps = sampler.finish().await?;

    eprintln!("owner loss: writes recovered {window:?} after the owner node died");
    assert_eq!(overlaps, 0, "both services claimed one shard at the same time");
    assert!(
        window >= ShardLeaseTtl::try_from(SHARD_LEASE_TTL)?.fence_after(),
        "the standby wrote before the old owner's fence ran out: {window:?}"
    );
    assert!(first.writer_shards().is_empty() && first.owned_shards().is_empty());
    cluster.restart(Peer::N1).await?;
    shutdown(second).await?;
    shutdown(first).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "three-node cluster fault test, run through `mise run presence:cluster`"]
async fn losing_the_stream_leader_pauses_writes_and_keeps_receipts_exact() -> TestResult {
    let mut cluster = cluster_or_skip!();
    let admin = cluster.client(Peer::N1).await?;
    provision(&admin).await?;
    let stream = presence_stream()?;
    let leader = Cluster::wait_for_replicas(&admin, &stream, REPLICAS_CURRENT).await?;
    let survivor = leader.others().next().ok_or("no surviving peer")?;
    let client = cluster.client(survivor).await?;
    let handle = Arc::new(start(client.clone(), "edge-a").await?);
    owning_all(&handle).await?;
    let writer = Writer { client: client.clone() };
    let mut landed = Vec::with_capacity(20);
    for index in 0..5 {
        landed.push((writer.land_once(&format!("k{index}")).await?.0, Instant::now()));
    }

    cluster.stop(leader);
    let killed = Instant::now();
    writer.first_landing("resumed", WRITES_RECOVER).await?;
    let window = killed.elapsed();
    steady(&client, &handle).await?;
    let sampler = ChurnSampler::spawn(handle.clone());
    let written = Instant::now();
    for index in 5..20 {
        landed.push((writer.land_once(&format!("k{index}")).await?.0, Instant::now()));
    }
    let churn = sampler.finish().await?;
    eprintln!(
        "stream leader loss: after steady state, {churn} over {:.2?} of writes",
        written.elapsed()
    );
    let elected = Cluster::wait_for_leader(&client, &stream, Some(leader), NEW_LEADER).await?;
    cluster.restart(leader).await?;
    Cluster::wait_for_replicas(&client, &stream, REPLICAS_CURRENT).await?;
    eprintln!("stream leader loss: {leader} to {elected}, writes resumed after {window:?}");

    let presence = Presence::open(client.clone(), presence_config()?).await?;
    let view = presence.watch(TOPIC.parse()?).await?;
    let state = view.state();
    let lease = presence_config()?.lease_ttl().get();
    for (intent, at) in &landed {
        let entries = state.get(&intent.key).map_or(0, <[_]>::len);
        assert!(entries <= 1, "{} has {entries} entries", intent.key);
        if at.elapsed() + EXPIRY_MARGIN < lease {
            assert_eq!(entries, 1, "{} is missing before its lease expired", intent.key);
        }
    }
    shutdown(handle).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "three-node cluster fault test, run through `mise run presence:cluster`"]
async fn losing_quorum_refuses_writes_with_a_typed_error_and_recovers() -> TestResult {
    let mut cluster = cluster_or_skip!();
    let client = cluster.client(Peer::N1).await?;
    provision(&client).await?;
    let handle = start(client.clone(), "edge-a").await?;
    owning_all(&handle).await?;
    let writer = Writer { client };
    let tracked = Intent::new("tracked")?;
    let receipt = writer.land(&tracked, WRITES_RECOVER).await?;

    cluster.stop(Peer::N2);
    cluster.stop(Peer::N3);
    let lost = Instant::now();
    let refused = QUORUM_REFUSAL
        .until(|| async {
            let intent = Intent::new("refused").ok()?;
            let reply = writer.track(&intent).await.ok()?;
            assert_ne!(reply.code(), "ok", "a write was acknowledged without quorum");
            UNAVAILABLE.contains(&reply.code()).then(|| reply.code().to_owned())
        })
        .await?;
    let beat = writer.heartbeat(&tracked, &receipt).await?;
    match &beat {
        Reply::Coded(code, body) if code == "ok" => {
            assert_eq!(
                body["entries"],
                json!(["unavailable"]),
                "heartbeat without quorum: {body}"
            );
        }
        Reply::Coded(code, body) => {
            assert!(
                UNAVAILABLE.contains(&code.as_str()),
                "heartbeat without quorum: {code} {body}"
            );
        }
        Reply::Silent => return Err("heartbeat without quorum got no reply".into()),
    }
    let refusal = lost.elapsed();

    cluster.restart(Peer::N2).await?;
    cluster.restart(Peer::N3).await?;
    let window = writer.first_landing("recovered", WRITES_RECOVER).await?;
    owning_all(&handle).await?;
    let beat = WRITES_RECOVER
        .until(|| async {
            match writer.heartbeat(&tracked, &receipt).await.ok()? {
                Reply::Coded(code, body) if code == "ok" && body["entries"] != json!(["unavailable"]) => Some(body),
                _ => None,
            }
        })
        .await?;
    eprintln!("quorum loss: refused with {refused} after {refusal:?}, writes resumed {window:?} after quorum returned");
    assert_eq!(
        beat["entries"],
        json!(["ok"]),
        "the tracked entry did not survive quorum loss"
    );
    tokio::time::timeout(Duration::from_secs(60), handle.shutdown())
        .await
        .map_err(|_| "service shutdown did not finish")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "three-node cluster fault test, run through `mise run presence:cluster`"]
async fn a_watcher_replays_through_a_leader_change_without_gaps_or_duplicates() -> TestResult {
    let mut cluster = cluster_or_skip!();
    let admin = cluster.client(Peer::N1).await?;
    provision(&admin).await?;
    let stream = presence_stream()?;
    let leader = Cluster::wait_for_replicas(&admin, &stream, REPLICAS_CURRENT).await?;
    let survivor = leader.others().next().ok_or("no surviving peer")?;
    let client = cluster.client(survivor).await?;
    let handle = Arc::new(start(client.clone(), "edge-a").await?);
    owning_all(&handle).await?;

    let presence = Presence::open(cluster.client(survivor).await?, presence_config()?).await?;
    let view = presence.watch(TOPIC.parse()?).await?;
    Wait::new("the watcher to be ready", Duration::from_secs(30))
        .until(|| async { (view.readiness() == Readiness::Ready).then_some(()) })
        .await?;
    let generation = view.generation();
    let (snapshot, mut diffs) = view.sync_state();
    let writer = Writer { client: client.clone() };
    let mut expected = BTreeSet::new();
    let mut landed_at = BTreeMap::new();
    let mut sampler = None;
    for index in 0..15 {
        if index == 5 {
            cluster.stop(leader);
            writer.first_landing("w-resumed", WRITES_RECOVER).await?;
            steady(&client, &handle).await?;
            sampler = Some((ChurnSampler::spawn(handle.clone()), Instant::now()));
        }
        let (intent, _) = writer.land_once(&format!("w{index}")).await?;
        landed_at.insert(intent.key.clone(), Instant::now());
        expected.insert(intent.key);
    }
    if let Some((sampler, written)) = sampler {
        let churn = sampler.finish().await?;
        eprintln!(
            "watcher: after steady state, {churn} over {:.2?} of writes",
            written.elapsed()
        );
    }
    cluster.restart(leader).await?;

    let mut joins: BTreeMap<PresenceKey, usize> = BTreeMap::new();
    let mut cursor = snapshot.cursor();
    let mut rebases = 0_u64;
    let deadline = Instant::now() + WATCH_CATCHES_UP.within;
    while !expected.iter().all(|key| joins.contains_key(key)) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let diff = match tokio::time::timeout(remaining, diffs.recv()).await {
            Ok(Ok(diff)) => diff,
            Ok(Err(RecvError::Lagged(skipped))) => return Err(format!("watcher lagged by {skipped} diffs").into()),
            Ok(Err(RecvError::Closed)) => return Err("watcher closed".into()),
            Err(_) => return Err(format!("timed out waiting for {}: saw {joins:?}", WATCH_CATCHES_UP.what).into()),
        };
        let step = cursor.follow(&diff.cursor(), diff.prev());
        match step {
            CursorStep::Next => {}
            CursorStep::Rebase => {
                rebases += 1;
                assert!(
                    view.counters().reconnects() >= rebases,
                    "diff after {cursor:?} rebased without a reconnect"
                );
            }
            CursorStep::Repeat | CursorStep::Gap | CursorStep::Stale => {
                panic!("diff after {cursor:?} stepped {step:?}")
            }
        }
        cursor = diff.cursor();
        let lease = presence_config()?.lease_ttl().get();
        for (key, _) in diff.diff().leaves().iter() {
            let expired = landed_at
                .get(key)
                .is_some_and(|at: &Instant| at.elapsed() + EXPIRY_MARGIN >= lease);
            assert!(expired, "{key} left before its lease expired: {:?}", diff.diff());
        }
        for (key, entries) in diff.diff().joins().iter() {
            *joins.entry(key.clone()).or_default() += entries.len();
        }
    }
    let reconnects = view.counters().reconnects();
    eprintln!("watcher through leader change: {reconnects} reconnects, {rebases} rebases, generation kept");
    assert!(joins.values().all(|count| *count == 1), "duplicate joins: {joins:?}");
    assert_eq!(view.generation(), generation, "the watcher changed generation");
    assert_eq!(presence.generation(), generation, "the stream changed generation");
    shutdown(handle).await
}
