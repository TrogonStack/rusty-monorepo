#[allow(dead_code)]
mod common;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_nats::header::NATS_MARKER_REASON;
use async_nats::jetstream::consumer::pull::OrderedConfig;
use async_nats::jetstream::stream::{self, External, LastRawMessageErrorKind, Source, StorageType};
use async_nats::jetstream::{self, message::StreamMessage};
use async_nats::{Message, Subscriber};
use futures_util::StreamExt;
use serde_json::{json, Value};

use trogon_presence::{
    ConnectionId, DiffSequence, EntryKey, HeartbeatInterval, HolderId, JetStreamDomain, LeaseTtl, MarkerTtl, Meta,
    OpenError, Presence, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic, WriteOutcome, WriteReceipt,
};
use trogon_presence_service::reply::{HEADER_CODE, HEADER_KIND, HEADER_PREV, HEADER_SEQ};
use trogon_presence_service::subjects::{diff_subject, HolderOp};
use trogon_presence_service::{
    KeepaliveInterval, NodeId, PresenceReader, ReadOp, ReaderIdentity, ReaderOptions, ServiceConfig, ServiceHandle,
    ShardLeaseTtl, WriteOp,
};

use common::domain::{DomainPair, ServerVersion, Side};
use common::BoxError;

type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const KEY: &str = "ana@x.io";
const ENTRY_LEASE: Duration = Duration::from_secs(1);
const ENTRY_HEARTBEAT: Duration = Duration::from_millis(400);
const MARKER_TTL: Duration = Duration::from_secs(5);
const SAMPLE_EVERY: Duration = Duration::from_millis(25);
const SOURCE_GONE_WITHIN: Duration = Duration::from_secs(20);
const RETENTION_CHECK: Duration = Duration::from_secs(3);
const MIRROR_LAG_BOUND: Duration = Duration::from_secs(1);
const MIRROR_CATCH_UP: Duration = Duration::from_secs(10);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(3);
const EXPIRY_SLACK: Duration = Duration::from_secs(1);
const SERVICE_LEASE: Duration = Duration::from_secs(60);
const SERVICE_HEARTBEAT: Duration = Duration::from_secs(10);
const SERVICE_MARKER_TTL: Duration = Duration::from_secs(120);
const SHARD_LEASE_TTL: Duration = Duration::from_secs(3);
const KEEPALIVE: Duration = Duration::from_secs(1);
const OWNERSHIP: Duration = Duration::from_secs(30);
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_secs(3);

macro_rules! pair_or_skip {
    () => {
        match DomainPair::start().await {
            Some(pair) => pair,
            None => return Ok(()),
        }
    };
}

fn presence_config() -> Result<PresenceConfig, BoxError> {
    Ok(PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(ENTRY_LEASE)?,
        HeartbeatInterval::try_from(ENTRY_HEARTBEAT)?,
        MarkerTtl::try_from(MARKER_TTL)?,
        ShardCount::DEFAULT,
    )?)
}

fn meta() -> Result<Meta, serde_json::Error> {
    serde_json::from_value(serde_json::json!({ "status": "online" }))
}

/// Whether the leaf mirror honors the per-message TTL header of what it copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorTtl {
    Honored,
    Ignored,
}

impl MirrorTtl {
    fn allows(self) -> bool {
        matches!(self, Self::Honored)
    }

    fn stream_name(self, source: &str) -> String {
        match self {
            Self::Honored => format!("{source}_MIRROR_TTL"),
            Self::Ignored => format!("{source}_MIRROR_NO_TTL"),
        }
    }
}

/// What a last-by-subject reader sees for the entry subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Entry(u64),
    Marker(u64),
    Absent,
}

impl Phase {
    fn of(message: &StreamMessage) -> Self {
        match message.headers.get(NATS_MARKER_REASON.as_ref()) {
            Some(_) => Self::Marker(message.sequence),
            None => Self::Entry(message.sequence),
        }
    }
}

/// One stored message as a consumer reader receives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Entry(u64),
    Marker(u64),
}

/// The phase changes one side went through, each with the time since the track was acknowledged.
#[derive(Debug, Default, Clone)]
struct Lifecycle {
    phases: Vec<(Phase, Duration)>,
}

impl Lifecycle {
    /// Records a phase change. Absence before the first entry is replication not yet arrived, not a phase.
    fn observe(&mut self, phase: Phase, at: Duration) {
        if self.phases.is_empty() && phase == Phase::Absent {
            return;
        }
        if self.phases.last().map(|(last, _)| *last) != Some(phase) {
            self.phases.push((phase, at));
        }
    }

    fn sequence(&self) -> Vec<Phase> {
        self.phases.iter().map(|(phase, _)| *phase).collect()
    }

    fn reached(&self, phase: Phase) -> Option<Duration> {
        self.phases.iter().find(|(seen, _)| *seen == phase).map(|(_, at)| *at)
    }

    /// The largest delay of any phase change here behind the same change in `source`.
    fn lag_behind(&self, source: &Self) -> Option<Duration> {
        self.phases
            .iter()
            .map(|(phase, at)| source.reached(*phase).map(|origin| at.saturating_sub(origin)))
            .collect::<Option<Vec<_>>>()
            .map(|lags| lags.into_iter().fold(Duration::ZERO, Duration::max))
    }
}

impl fmt::Display for Lifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (phase, at) in &self.phases {
            write!(f, "[{phase:?} at {at:.2?}]")?;
        }
        Ok(())
    }
}

/// How the mirror's view of one expiring entry relates to the source's.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MirrorOutcome {
    /// Same phases, same sequences, same deliveries.
    Faithful,
    /// The marker arrives but outlives its own TTL on the mirror.
    MarkerRetained,
    /// The entry disappears on the mirror's own timer but the marker never arrives.
    MarkerDropped,
    /// The entry outlives its TTL on the mirror and the marker never arrives.
    EntryRetained,
    Unclassified(String),
}

struct Observation {
    source: Lifecycle,
    mirror: Lifecycle,
    source_deliveries: Vec<Delivery>,
    mirror_deliveries: Vec<Delivery>,
    source_last_sequence: u64,
    mirror_last_sequence: u64,
}

impl Observation {
    fn classify(&self) -> MirrorOutcome {
        let source = self.source.sequence();
        let mirror = self.mirror.sequence();
        let entry_only: Vec<Delivery> = self
            .source_deliveries
            .iter()
            .copied()
            .filter(|delivery| matches!(delivery, Delivery::Entry(_)))
            .collect();
        let entry = source.first().copied();
        if mirror == source && self.mirror_deliveries == self.source_deliveries {
            return MirrorOutcome::Faithful;
        }
        if self.mirror_deliveries == self.source_deliveries
            && source.last() == Some(&Phase::Absent)
            && mirror.as_slice() == &source[..source.len() - 1]
        {
            return MirrorOutcome::MarkerRetained;
        }
        if self.mirror_deliveries == entry_only {
            if let Some(entry) = entry {
                if mirror == [entry, Phase::Absent] {
                    return MirrorOutcome::MarkerDropped;
                }
                if mirror == [entry] {
                    return MirrorOutcome::EntryRetained;
                }
            }
        }
        MirrorOutcome::Unclassified(self.to_string())
    }
}

impl fmt::Display for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "source {} deliveries {:?} last seq {}; mirror {} deliveries {:?} last seq {}",
            self.source,
            self.source_deliveries,
            self.source_last_sequence,
            self.mirror,
            self.mirror_deliveries,
            self.mirror_last_sequence
        )
    }
}

/// The recorded verdict for one pinned release and mirror setting.
fn expected_outcome(version: &ServerVersion, ttl: MirrorTtl) -> Result<MirrorOutcome, BoxError> {
    match (version.as_str(), ttl) {
        ("2.14.7", MirrorTtl::Honored) => Ok(MirrorOutcome::Faithful),
        ("2.14.7", MirrorTtl::Ignored) => Ok(MirrorOutcome::MarkerRetained),
        ("2.12.15", MirrorTtl::Honored) => Ok(MirrorOutcome::MarkerDropped),
        ("2.12.15", MirrorTtl::Ignored) => Ok(MirrorOutcome::EntryRetained),
        (other, _) => Err(format!("nats-server {other} has no recorded mirror verdict; measure and record one").into()),
    }
}

async fn last_phase(stream: &stream::Stream, subject: &str) -> Result<Phase, BoxError> {
    match stream.get_last_raw_message_by_subject(subject).await {
        Ok(message) => Ok(Phase::of(&message)),
        Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(Phase::Absent),
        Err(err) => Err(err.into()),
    }
}

/// Every message an ordered consumer delivered on one subject, in delivery order.
struct DeliveryLog {
    seen: Arc<Mutex<Vec<Delivery>>>,
    task: tokio::task::JoinHandle<Result<(), BoxError>>,
}

impl DeliveryLog {
    fn stop(self) -> Result<Vec<Delivery>, BoxError> {
        self.task.abort();
        Ok(self.seen.lock().map_err(|_| "delivery log poisoned")?.clone())
    }
}

fn record_deliveries(stream: stream::Stream, subject: String) -> DeliveryLog {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let task = tokio::spawn(async move {
        let consumer = stream
            .create_consumer(OrderedConfig {
                filter_subject: subject,
                ..Default::default()
            })
            .await?;
        let mut messages = consumer.messages().await?;
        while let Some(message) = messages.next().await {
            let message = message?;
            let sequence = message.info()?.stream_sequence;
            let marker = message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(NATS_MARKER_REASON.as_ref()))
                .is_some();
            let delivery = if marker {
                Delivery::Marker(sequence)
            } else {
                Delivery::Entry(sequence)
            };
            sink.lock().map_err(|_| "delivery log poisoned")?.push(delivery);
        }
        Ok(())
    });
    DeliveryLog { seen, task }
}

async fn create_mirror(leaf: &jetstream::Context, source: &str, ttl: MirrorTtl) -> Result<stream::Stream, BoxError> {
    Ok(leaf
        .create_stream(stream::Config {
            name: ttl.stream_name(source),
            mirror: Some(Source {
                name: source.to_owned(),
                external: Some(External {
                    api_prefix: Side::Hub.api_prefix(),
                    delivery_prefix: None,
                }),
                ..Default::default()
            }),
            max_messages_per_subject: 1,
            allow_message_ttl: ttl.allows(),
            storage: StorageType::File,
            ..Default::default()
        })
        .await?)
}

/// Waits until the mirror is active and holds the source's last sequence, so the measured lag is the
/// steady-state replication delay and not the mirror's startup.
async fn mirror_caught_up(source: &stream::Stream, mirror: &stream::Stream) -> TestResult {
    let started = Instant::now();
    loop {
        let mut source = source.clone();
        let wanted = source.info().await?.state.last_sequence;
        let mut mirror = mirror.clone();
        let info = mirror.info().await?;
        let active = info
            .mirror
            .as_ref()
            .is_some_and(|mirror| mirror.active.is_some() && mirror.lag == 0);
        if active && info.state.last_sequence == wanted {
            return Ok(());
        }
        if started.elapsed() >= MIRROR_CATCH_UP {
            return Err(format!(
                "the mirror did not catch up within {MIRROR_CATCH_UP:?}: {:?}",
                info.mirror
            )
            .into());
        }
        tokio::time::sleep(SAMPLE_EVERY).await;
    }
}

/// Tracks one entry in the hub bucket without heartbeats, then samples the hub stream and a leaf mirror
/// of it until the source has dropped the entry and its marker, plus a retention check.
async fn observe_expiry(pair: &DomainPair, ttl: MirrorTtl) -> Result<(ServerVersion, Observation), BoxError> {
    let hub_client = pair.client(Side::Hub).await?;
    let leaf_client = pair.client(Side::Leaf).await?;
    let version = ServerVersion::of(&leaf_client);
    let config = presence_config()?;
    let presence = Presence::provision(hub_client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let source_name = config.bucket().stream_name();
    let source = jetstream::new(hub_client.clone()).get_stream(&source_name).await?;
    let mirror = create_mirror(&DomainPair::context(&leaf_client, Side::Leaf), &source_name, ttl).await?;

    let holder = HolderId::generate()?;
    let entry = EntryKey::new(TOPIC.parse()?, KEY.parse()?, holder);
    let subject = config.bucket().subject_for(&entry.encode(config.shards())?);
    let source_log = record_deliveries(source.clone(), subject.clone());
    let mirror_log = record_deliveries(mirror.clone(), subject.clone());

    mirror_caught_up(&source, &mirror).await?;
    let tracker = presence.tracker_for(holder)?;
    presence.close();
    let outcome = tracker.track(&TOPIC.parse()?, &KEY.parse()?, meta()?).await?;
    let tracked = Instant::now();
    if !matches!(outcome, WriteOutcome::Applied(_)) {
        return Err(format!("track did not apply: {outcome:?}").into());
    }

    let mut source_life = Lifecycle::default();
    let mut mirror_life = Lifecycle::default();
    let mut source_gone: Option<Instant> = None;
    loop {
        let at = tracked.elapsed();
        let seen = last_phase(&source, &subject).await?;
        source_life.observe(seen, at);
        mirror_life.observe(last_phase(&mirror, &subject).await?, at);
        if seen == Phase::Absent && source_life.phases.len() > 1 {
            source_gone.get_or_insert_with(Instant::now);
        }
        match source_gone {
            Some(gone) if gone.elapsed() >= RETENTION_CHECK => break,
            None if at >= SOURCE_GONE_WITHIN => {
                return Err(format!("the source never dropped the entry: {source_life}").into());
            }
            _ => {}
        }
        tokio::time::sleep(SAMPLE_EVERY).await;
    }
    let source_deliveries = source_log.stop()?;
    let mirror_deliveries = mirror_log.stop()?;
    let source_last_sequence = source.clone().info().await?.state.last_sequence;
    let mirror_last_sequence = mirror.clone().info().await?.state.last_sequence;
    Ok((
        version,
        Observation {
            source: source_life,
            mirror: mirror_life,
            source_deliveries,
            mirror_deliveries,
            source_last_sequence,
            mirror_last_sequence,
        },
    ))
}

async fn assert_mirror_verdict(ttl: MirrorTtl) -> TestResult {
    let pair = pair_or_skip!();
    let (version, observation) = observe_expiry(&pair, ttl).await?;
    let expected = expected_outcome(&version, ttl)?;
    let found = observation.classify();
    eprintln!("mirror ttl {ttl:?} on nats-server {version}: {found:?}; {observation}");
    assert_eq!(found, expected, "nats-server {version} mirror {ttl:?}: {observation}");
    match found {
        MirrorOutcome::Faithful => {
            let lag = observation
                .mirror
                .lag_behind(&observation.source)
                .ok_or("the mirror reached a phase the source never had")?;
            eprintln!("mirror ttl {ttl:?} on nats-server {version}: largest phase lag {lag:?}");
            assert!(lag <= MIRROR_LAG_BOUND, "mirror lagged {lag:?}: {observation}");
            assert_eq!(observation.mirror_last_sequence, observation.source_last_sequence);
        }
        MirrorOutcome::MarkerRetained => {
            let marker = observation
                .source_deliveries
                .iter()
                .find_map(|delivery| match delivery {
                    Delivery::Marker(sequence) => Some(*sequence),
                    Delivery::Entry(_) => None,
                })
                .ok_or("no marker delivered")?;
            assert_eq!(
                observation.mirror.sequence().last(),
                Some(&Phase::Marker(marker)),
                "the mirror should still hold the marker past its ttl"
            );
        }
        MirrorOutcome::MarkerDropped => {
            assert!(
                observation.mirror_last_sequence < observation.source_last_sequence,
                "the mirror cursor should stop before the marker: {observation}"
            );
        }
        MirrorOutcome::EntryRetained => {
            assert!(
                matches!(observation.mirror.sequence().last(), Some(Phase::Entry(_))),
                "the mirror should still hold the expired entry: {observation}"
            );
        }
        MirrorOutcome::Unclassified(_) => {}
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn a_leaf_mirror_honoring_ttl_matches_the_recorded_verdict() -> TestResult {
    assert_mirror_verdict(MirrorTtl::Honored).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn a_leaf_mirror_ignoring_ttl_matches_the_recorded_verdict() -> TestResult {
    assert_mirror_verdict(MirrorTtl::Ignored).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn a_mirror_refuses_subject_delete_markers_of_its_own() -> TestResult {
    let pair = pair_or_skip!();
    let leaf = DomainPair::context(&pair.client(Side::Leaf).await?, Side::Leaf);
    let refused = leaf
        .create_stream(stream::Config {
            name: "MARKER_MIRROR".to_owned(),
            mirror: Some(Source {
                name: "ANY".to_owned(),
                external: Some(External {
                    api_prefix: Side::Hub.api_prefix(),
                    delivery_prefix: None,
                }),
                ..Default::default()
            }),
            allow_message_ttl: true,
            subject_delete_marker_ttl: Some(MARKER_TTL),
            ..Default::default()
        })
        .await;
    let err = refused.err().ok_or("a mirror accepted subject delete markers")?;
    assert!(
        err.to_string().contains("subject delete markers forbidden on mirrors"),
        "unexpected refusal: {err}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn kv_subjects_cross_domains_only_through_the_api_prefix() -> TestResult {
    let pair = pair_or_skip!();
    let hub = jetstream::new(pair.client(Side::Hub).await?);
    hub.create_stream(stream::Config {
        name: "KV_DOMAIN_PROBE".to_owned(),
        subjects: vec!["$KV.DOMAIN_PROBE.>".to_owned()],
        ..Default::default()
    })
    .await?;
    let leaf = pair.client(Side::Leaf).await?;
    let direct = tokio::time::timeout(PUBLISH_TIMEOUT, leaf.request("$KV.DOMAIN_PROBE.direct", "x".into())).await;
    assert!(
        !matches!(direct, Ok(Ok(_))),
        "a raw $KV publish from the leaf reached the hub stream"
    );
    let prefixed = format!("{}.$KV.DOMAIN_PROBE.prefixed", Side::Hub.api_prefix());
    let ack = leaf.request(prefixed, "x".into()).await?;
    let body: serde_json::Value = serde_json::from_slice(&ack.payload)?;
    assert_eq!(body["stream"], "KV_DOMAIN_PROBE", "unexpected ack {body}");
    assert_eq!(body["domain"], Side::Hub.domain(), "unexpected ack {body}");
    let stored = hub
        .get_stream("KV_DOMAIN_PROBE")
        .await?
        .get_last_raw_message_by_subject("$KV.DOMAIN_PROBE.prefixed")
        .await?;
    assert_eq!(stored.subject.as_str(), "$KV.DOMAIN_PROBE.prefixed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn a_leaf_client_without_a_domain_cannot_open_the_hub_bucket() -> TestResult {
    let pair = pair_or_skip!();
    let config = presence_config()?;
    Presence::provision(
        pair.client(Side::Hub).await?,
        config.clone(),
        ProvisionOptions::default(),
    )
    .await?;
    match Presence::open(pair.client(Side::Leaf).await?, config).await {
        Err(OpenError::BucketMissing(_)) => Ok(()),
        Err(other) => Err(format!("expected a missing bucket through the leaf, got {other}").into()),
        Ok(_) => Err("the leaf opened the hub bucket without a domain".into()),
    }
}

/// How a client reaches the hub bucket: connected to the hub itself, or to the leaf through the hub domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    Direct,
    ThroughLeaf,
}

impl Path {
    const ALL: [Self; 2] = [Self::Direct, Self::ThroughLeaf];

    fn side(self) -> Side {
        match self {
            Self::Direct => Side::Hub,
            Self::ThroughLeaf => Side::Leaf,
        }
    }

    fn route(self, config: PresenceConfig) -> Result<PresenceConfig, BoxError> {
        Ok(match self {
            Self::Direct => config,
            Self::ThroughLeaf => config.with_domain(Side::Hub.domain().parse::<JetStreamDomain>()?),
        })
    }
}

async fn hub_stream(pair: &DomainPair, config: &PresenceConfig) -> Result<stream::Stream, BoxError> {
    Ok(jetstream::new(pair.client(Side::Hub).await?)
        .get_stream(config.bucket().stream_name())
        .await?)
}

/// What one library write path left in the hub bucket: both receipts, the entry's phases until its marker is
/// gone, and the stream's last sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpiryTranscript {
    tracked: (u64, String),
    updated: (u64, String),
    phases: Vec<Phase>,
    last_sequence: u64,
}

fn applied(outcome: WriteOutcome) -> Result<WriteReceipt, BoxError> {
    match outcome {
        WriteOutcome::Applied(receipt) => Ok(receipt),
        other => Err(format!("write did not apply: {other:?}").into()),
    }
}

fn receipt_of(receipt: &WriteReceipt) -> (u64, String) {
    (receipt.revision().get(), format!("{:?}", receipt.sequence()))
}

/// Tracks and updates one entry through `path` without heartbeats, then samples the hub stream until the
/// entry expired, its marker was written and the marker expired too.
async fn expire_through(path: Path) -> Result<Option<(ExpiryTranscript, Lifecycle)>, BoxError> {
    let Some(pair) = DomainPair::start().await else {
        return Ok(None);
    };
    let config = path.route(presence_config()?)?;
    let client = pair.client(path.side()).await?;
    let presence = Presence::provision(client, config.clone(), ProvisionOptions::default()).await?;
    let source = hub_stream(&pair, &config).await?;
    let holder = HolderId::generate()?;
    let subject = config
        .bucket()
        .subject_for(&EntryKey::new(TOPIC.parse()?, KEY.parse()?, holder).encode(config.shards())?);
    let tracker = presence.tracker_for(holder)?;
    presence.close();
    let topic: Topic = TOPIC.parse()?;
    let key: PresenceKey = KEY.parse()?;
    let tracked = applied(tracker.track(&topic, &key, meta()?).await?)?;
    let updated = applied(
        tracker
            .update(&topic, &key, serde_json::from_value(json!({ "status": "busy" }))?)
            .await?,
    )?;
    let written = Instant::now();
    let mut life = Lifecycle::default();
    loop {
        let at = written.elapsed();
        let seen = last_phase(&source, &subject).await?;
        life.observe(seen, at);
        if seen == Phase::Absent && life.phases.len() > 1 {
            break;
        }
        if at >= SOURCE_GONE_WITHIN {
            return Err(format!("{path:?}: the hub never dropped the entry: {life}").into());
        }
        tokio::time::sleep(SAMPLE_EVERY).await;
    }
    let last_sequence = source.clone().info().await?.state.last_sequence;
    Ok(Some((
        ExpiryTranscript {
            tracked: receipt_of(&tracked),
            updated: receipt_of(&updated),
            phases: life.sequence(),
            last_sequence,
        },
        life,
    )))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn ttl_markers_and_batch_receipts_through_the_leaf_match_a_direct_hub_run() -> TestResult {
    let Some((direct, direct_life)) = expire_through(Path::Direct).await? else {
        return Ok(());
    };
    let Some((leaf, leaf_life)) = expire_through(Path::ThroughLeaf).await? else {
        return Ok(());
    };
    eprintln!("expiry direct: {direct:?} {direct_life}");
    eprintln!("expiry through leaf: {leaf:?} {leaf_life}");
    assert_eq!(leaf, direct, "the leaf path diverged from the direct path");
    assert!(
        matches!(
            direct.phases.as_slice(),
            [Phase::Entry(entry), Phase::Marker(marker), Phase::Absent]
                if *entry == direct.updated.0 && *marker == direct.updated.0 + 1
        ),
        "unexpected phases {direct:?}"
    );
    for life in [&direct_life, &leaf_life] {
        let marker = life
            .phases
            .iter()
            .find_map(|(phase, at)| matches!(phase, Phase::Marker(_)).then_some(*at))
            .ok_or("no marker was observed")?;
        assert!(
            marker <= ENTRY_LEASE + EXPIRY_SLACK,
            "the marker arrived {marker:?} after the write, beyond {:?}",
            ENTRY_LEASE + EXPIRY_SLACK
        );
    }
    Ok(())
}

fn service_config(path: Path) -> Result<ServiceConfig, BoxError> {
    let presence = path.route(PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(SERVICE_LEASE)?,
        HeartbeatInterval::try_from(SERVICE_HEARTBEAT)?,
        MarkerTtl::try_from(SERVICE_MARKER_TTL)?,
        ShardCount::DEFAULT,
    )?)?;
    Ok(ServiceConfig::new(presence, "edge-a".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(SHARD_LEASE_TTL)?)
        .with_keepalive(KeepaliveInterval::try_from(KEEPALIVE)?))
}

async fn start_service(client: async_nats::Client, config: ServiceConfig) -> Result<ServiceHandle, BoxError> {
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    let expected = usize::from(ShardCount::DEFAULT.get());
    tokio::time::timeout(OWNERSHIP, async {
        while handle.owned_shards().len() != expected {
            tokio::time::sleep(SAMPLE_EVERY).await;
        }
    })
    .await
    .map_err(|_| "the service never owned every shard")?;
    common::wait_for_writers(&handle, expected, OWNERSHIP).await?;
    Ok(handle)
}

fn header<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    message
        .headers
        .as_ref()
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn number(message: &Message, name: &str) -> Result<u64, BoxError> {
    Ok(header(message, name)
        .ok_or_else(|| format!("missing {name}"))?
        .parse()?)
}

fn keys(body: &Value, side: &str) -> BTreeSet<String> {
    object_keys(&body[side])
}

fn object_keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .into_iter()
        .flat_map(|map| map.keys().cloned())
        .collect()
}

/// One diff frame as a watcher applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffStep {
    seq: u64,
    joins: BTreeSet<String>,
    leaves: BTreeSet<String>,
}

/// Follows the diff frames of one topic and refuses any gap, duplicate or reordering.
struct Watcher {
    frames: Subscriber,
    last_seq: u64,
    diffs: Vec<DiffStep>,
}

impl Watcher {
    async fn subscribe(client: &async_nats::Client, topic: &Topic) -> Result<Self, BoxError> {
        let frames = client.subscribe(diff_subject(topic)).await?;
        client.flush().await?;
        Ok(Self {
            frames,
            last_seq: DiffSequence::FIRST.get(),
            diffs: Vec::new(),
        })
    }

    fn apply(&mut self, message: &Message) -> Result<Option<DiffStep>, BoxError> {
        let prev = number(message, HEADER_PREV)?;
        let seq = number(message, HEADER_SEQ)?;
        match header(message, HEADER_KIND) {
            Some("diff") => {
                assert_eq!(prev, self.last_seq, "a diff does not chain to the previous one");
                assert_eq!(seq, prev + 1, "a diff skipped or repeated a sequence");
                self.last_seq = seq;
                let body: Value = serde_json::from_slice(&message.payload)?;
                let step = DiffStep {
                    seq,
                    joins: keys(&body, "joins"),
                    leaves: keys(&body, "leaves"),
                };
                self.diffs.push(step.clone());
                Ok(Some(step))
            }
            Some("keepalive") => {
                assert_eq!(
                    (prev, seq),
                    (self.last_seq, self.last_seq),
                    "a keepalive moved the cursor"
                );
                Ok(None)
            }
            other => Err(format!("unexpected frame kind {other:?}").into()),
        }
    }

    async fn next_diff(&mut self) -> Result<DiffStep, BoxError> {
        tokio::time::timeout(FRAME_TIMEOUT, async {
            loop {
                let message = self.frames.next().await.ok_or("frame subscription ended")?;
                if let Some(step) = self.apply(&message)? {
                    return Ok::<_, BoxError>(step);
                }
            }
        })
        .await
        .map_err(|_| format!("no diff within {FRAME_TIMEOUT:?}"))?
    }

    async fn quiet(&mut self) -> TestResult {
        let until = Instant::now() + QUIET;
        while let Ok(Some(message)) = tokio::time::timeout_at(until.into(), self.frames.next()).await {
            if let Some(step) = self.apply(&message)? {
                return Err(format!("unexpected diff {step:?}").into());
            }
        }
        Ok(())
    }
}

/// One reply from the service: its code and the receipt positions it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answer {
    op: &'static str,
    code: String,
    rev: Value,
    mutation_seq: Value,
}

async fn send(
    client: &async_nats::Client,
    op: &'static str,
    subject: String,
    key: &PresenceKey,
    body: &Value,
) -> Result<(Answer, Value), BoxError> {
    let reply = common::command(client, subject, key, body).await?;
    let code = header(&reply, HEADER_CODE).ok_or("reply has no code")?.to_owned();
    let body = common::body_of(&reply)?;
    let answer = Answer {
        op,
        code,
        rev: body["rev"].clone(),
        mutation_seq: body["mutation_seq"].clone(),
    };
    Ok((answer, body))
}

/// Everything a writer, a watcher and a snapshot reader saw for one run against the hub bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceTranscript {
    answers: Vec<Answer>,
    diffs: Vec<DiffStep>,
    listed_seq: u64,
    listed: BTreeSet<String>,
    snapshot: BTreeSet<String>,
    last_sequence: u64,
}

async fn serve_through(path: Path) -> Result<Option<ServiceTranscript>, BoxError> {
    let Some(pair) = DomainPair::start().await else {
        return Ok(None);
    };
    let config = service_config(path)?;
    let handle = start_service(pair.client(path.side()).await?, config.clone()).await?;
    let client = pair.client(path.side()).await?;
    let topic: Topic = TOPIC.parse()?;
    let mut watcher = Watcher::subscribe(&client, &topic).await?;
    let ana: PresenceKey = "ana".parse()?;
    let bob: PresenceKey = "bob".parse()?;
    let ana_holder = HolderId::generate()?;
    let mut answers = Vec::new();

    let (answer, tracked) = send(
        &client,
        "track",
        WriteOp::Track.subject(&ana, &topic),
        &ana,
        &json!({ "holder": ana_holder, "meta": { "status": "online" } }),
    )
    .await?;
    answers.push(answer);
    watcher.next_diff().await?;

    let (answer, _) = send(
        &client,
        "track",
        WriteOp::Track.subject(&bob, &topic),
        &bob,
        &json!({ "holder": HolderId::generate()?, "meta": { "status": "away" } }),
    )
    .await?;
    answers.push(answer);
    watcher.next_diff().await?;

    let (answer, _) = send(
        &client,
        "heartbeat",
        HolderOp::Heartbeat.subject(&ana),
        &ana,
        &json!({ "entries": [{
            "holder": ana_holder,
            "topic": topic,
            "lifetime": tracked["lifetime"],
            "mutation_seq": tracked["mutation_seq"],
        }] }),
    )
    .await?;
    answers.push(answer);

    let (answer, updated) = send(
        &client,
        "update",
        WriteOp::Update.subject(&ana, &topic),
        &ana,
        &json!({
            "holder": ana_holder,
            "meta": { "status": "busy" },
            "lifetime": tracked["lifetime"],
            "mutation_seq": tracked["mutation_seq"],
        }),
    )
    .await?;
    answers.push(answer);
    watcher.next_diff().await?;

    let (answer, _) = send(
        &client,
        "untrack",
        WriteOp::Untrack.subject(&ana, &topic),
        &ana,
        &json!({ "holder": ana_holder, "lifetime": updated["lifetime"], "mutation_seq": updated["mutation_seq"] }),
    )
    .await?;
    answers.push(answer);
    watcher.next_diff().await?;
    watcher.quiet().await?;

    let caller: PresenceKey = "observer".parse()?;
    let listed = client
        .request(
            ReadOp::List.subject(ShardCount::DEFAULT, &caller, &topic),
            Vec::new().into(),
        )
        .await?;
    let listed_seq = number(&listed, HEADER_SEQ)?;
    let listed_state: Value = serde_json::from_slice(&listed.payload)?;
    let listed_keys = object_keys(&listed_state);

    let reader = PresenceReader::start(
        client.clone(),
        ReaderIdentity::new(caller, ConnectionId::generate()?),
        topic.clone(),
        ReaderOptions::default(),
    )
    .await?;
    let mut state = reader.watch();
    let wanted = BTreeSet::from([bob.to_string()]);
    let snapshot = tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let seen: BTreeSet<String> = state.borrow().iter().map(|(key, _)| key.to_string()).collect();
            if seen == wanted {
                return Ok::<_, BoxError>(seen);
            }
            state.changed().await?;
        }
    })
    .await
    .map_err(|_| "the snapshot reader never assembled the state")??;
    reader.close().await;

    let last_sequence = hub_stream(&pair, config.presence())
        .await?
        .info()
        .await?
        .state
        .last_sequence;
    handle.shutdown().await;
    Ok(Some(ServiceTranscript {
        answers,
        diffs: watcher.diffs,
        listed_seq,
        listed: listed_keys,
        snapshot,
        last_sequence,
    }))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hub and leaf domain fixture, run through `mise run presence:domain`"]
async fn a_service_on_the_leaf_serves_the_hub_bucket_like_a_direct_one() -> TestResult {
    let mut runs = Vec::new();
    for path in Path::ALL {
        let Some(transcript) = serve_through(path).await? else {
            return Ok(());
        };
        eprintln!("service {path:?}: {transcript:?}");
        runs.push(transcript);
    }
    let [direct, leaf] = runs.as_slice() else {
        return Err("expected one run per path".into());
    };
    assert!(
        direct.answers.iter().all(|answer| answer.code == "ok"),
        "a direct write was refused: {direct:?}"
    );
    assert_eq!(
        direct.diffs.iter().map(|step| step.seq).collect::<Vec<_>>(),
        (DiffSequence::FIRST.get() + 1..=DiffSequence::FIRST.get() + 4).collect::<Vec<_>>()
    );
    assert_eq!(direct.listed_seq, DiffSequence::FIRST.get() + 4);
    assert_eq!(direct.listed, BTreeSet::from(["bob".to_owned()]));
    assert_eq!(leaf, direct, "the leaf path diverged from the direct path");
    Ok(())
}
