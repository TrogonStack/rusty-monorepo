mod common;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::{
    AtomicBatch, BatchOutcome, BatchSink, GuardCapacity, HeartbeatInterval, HolderId, HolderLimit, LeaseTtl,
    ManagedError, ManagedLimits, MarkerTtl, Meta, NatsBatchSink, OwnerDeadline, OwnerEpoch, OwnerId, Presence,
    PresenceConfig, PresenceKey, ProvisionOptions, RateBurst, RateLimit, RetryWindow, ShardCount, SinkFuture,
    StreamGeneration, SuspendAwareClock, Topic, WriteOutcome, WriterMode, WriterShard,
};
use trogon_presence_service::subjects::{internal_write_subject, HolderOp};
use trogon_presence_service::{
    AdmissionLimits, Command, Forwarded, KeepaliveInterval, KeyQueueDepth, LeaseKey, LeaseStore, LeaseValue, NodeId,
    RequestIdentity, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
};

use common::{BoxError, NatsServer};

type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const LEASE_TTL: Duration = Duration::from_secs(3);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
const BURST_TIMEOUT: Duration = Duration::from_secs(10);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn presence_config() -> Result<PresenceConfig, BoxError> {
    Ok(PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(LEASE_TTL)?,
        HeartbeatInterval::try_from(Duration::from_secs(1))?,
        MarkerTtl::try_from(LEASE_TTL * 2)?,
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed))
}

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    Ok(ServiceConfig::new(presence_config()?, node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

fn generous() -> Result<AdmissionLimits, BoxError> {
    Ok(AdmissionLimits::default()
        .with_key_rate(RateLimit::new(Duration::from_millis(1), RateBurst::try_from(10_000)?)?))
}

fn meta(status: &str) -> Result<Meta, BoxError> {
    Ok(serde_json::from_value(json!({ "status": status }))?)
}

fn writer_count() -> usize {
    usize::from(ShardCount::DEFAULT.get())
}

async fn start(server: &NatsServer, config: ServiceConfig) -> Result<ServiceHandle, BoxError> {
    let client = server.client().await;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    common::wait_for_writers(&handle, writer_count(), OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

struct Probe {
    client: async_nats::Client,
}

impl Probe {
    async fn new(server: &NatsServer) -> Self {
        Self {
            client: server.client().await,
        }
    }

    async fn request(&self, subject: String, key: &str, body: Value) -> Result<(String, Value), BoxError> {
        let key: PresenceKey = key.parse()?;
        let reply = common::command(&self.client, subject, &key, &body).await?;
        let code = common::code_of(&reply).ok_or("reply has no code")?.to_owned();
        Ok((code, common::body_of(&reply)?))
    }

    async fn track(&self, key: &str, topic: &str, holder: HolderId) -> Result<(String, Value), BoxError> {
        self.request(
            WriteOp::Track.subject(&key.parse()?, &topic.parse()?),
            key,
            json!({ "holder": holder, "meta": { "status": "online" } }),
        )
        .await
    }

    async fn tracked(&self, key: &str, topic: &str, holder: HolderId) -> Result<Value, BoxError> {
        let (code, body) = self.track(key, topic, holder).await?;
        assert_eq!(code, "ok", "{key} on {topic}: {body}");
        Ok(body)
    }

    async fn holder_op(&self, op: HolderOp, key: &str, body: Value) -> Result<(String, Value), BoxError> {
        self.request(op.subject(&key.parse::<PresenceKey>()?), key, body).await
    }

    async fn burst(&self, key: &str, count: usize) -> Result<Vec<String>, BoxError> {
        let parsed: PresenceKey = key.parse()?;
        let prefix = format!(
            "{}.{}.{}",
            trogon_presence_service::CALLER_INBOX_PREFIX,
            parsed.token(),
            HolderId::generate()?
        );
        let mut replies = self.client.subscribe(format!("{prefix}.*")).await?;
        let holder = HolderId::generate()?;
        for index in 0..count {
            let topic: Topic = format!("room:burst{index}").parse()?;
            let body = json!({ "holder": holder, "meta": { "status": "online" } });
            self.client
                .publish_with_reply(
                    WriteOp::Track.subject(&parsed, &topic),
                    format!("{prefix}.{index}"),
                    serde_json::to_vec(&body)?.into(),
                )
                .await?;
        }
        self.client.flush().await?;
        let mut codes = Vec::with_capacity(count);
        tokio::time::timeout(BURST_TIMEOUT, async {
            while codes.len() < count {
                match replies.next().await {
                    Some(reply) => codes.push(common::code_of(&reply).unwrap_or("missing").to_owned()),
                    None => break,
                }
            }
        })
        .await
        .map_err(|_| format!("only {} of {count} burst replies arrived: {codes:?}", codes.len()))?;
        Ok(codes)
    }
}

fn count(codes: &[String], code: &str) -> usize {
    codes.iter().filter(|seen| seen.as_str() == code).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_lease_holder_admits_writes_and_peers_forward() -> TestResult {
    let server = server_or_skip!();
    let first = start(&server, service_config("edge-a")?).await?;
    let second = trogon_presence_service::start(server.client().await, service_config("edge-b")?).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        second.writer_shards().is_empty(),
        "second instance took held writer leases"
    );

    let probe = Probe::new(&server).await;
    let keys: Vec<String> = (0..24).map(|index| format!("user{index}")).collect();
    for key in &keys {
        probe.tracked(key, TOPIC, HolderId::generate()?).await?;
    }

    let (a, b) = (first.stats(), second.stats());
    assert_eq!(b.admitted(), 0, "an instance without writer leases admitted a write");
    assert_eq!(a.admitted(), keys.len() as u64);
    assert_eq!(a.forwarded() + b.forwarded(), keys.len() as u64);
    assert!(
        b.forwarded() > 0,
        "the second instance never took a write off the queue group"
    );

    second.shutdown().await;
    first.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_thirty_third_holder_is_refused() -> TestResult {
    let server = server_or_skip!();
    let handle = start(&server, service_config("edge-a")?.with_admission(generous()?)).await?;
    let probe = Probe::new(&server).await;
    for _ in 0..32 {
        probe.tracked("ana", TOPIC, HolderId::generate()?).await?;
    }
    let (code, body) = probe.track("ana", TOPIC, HolderId::generate()?).await?;
    assert_eq!(code, "holder_limit", "{body}");
    assert_eq!(body["retryable"], false, "{body}");
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sixty_fifth_topic_is_refused() -> TestResult {
    let server = server_or_skip!();
    let handle = start(&server, service_config("edge-a")?.with_admission(generous()?)).await?;
    let probe = Probe::new(&server).await;
    let holder = HolderId::generate()?;
    for index in 0..64 {
        probe.tracked("ana", &format!("room:t{index}"), holder).await?;
    }
    let (code, body) = probe.track("ana", "room:t64", holder).await?;
    assert_eq!(code, "topic_limit", "{body}");
    probe.tracked("ana", "room:t64", HolderId::generate()?).await?;
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guard_past_its_capacity_is_refused() -> TestResult {
    let server = server_or_skip!();
    let limits = ManagedLimits::default().with_capacity(GuardCapacity::try_from(1024)?);
    let config = service_config("edge-a")?
        .with_admission(generous()?)
        .with_managed_limits(limits);
    let handle = start(&server, config).await?;
    let probe = Probe::new(&server).await;
    let holder = HolderId::generate()?;
    let mut refused = None;
    for index in 0..32 {
        let topic = format!("room:{index:03}{}", "x".repeat(100));
        let (code, body) = probe.track("ana", &topic, holder).await?;
        if code != "ok" {
            refused = Some((index, code, body));
            break;
        }
    }
    let (index, code, body) = refused.ok_or("the guard never reached its capacity")?;
    assert_eq!(code, "identity_capacity_exceeded", "{body}");
    assert!(index > 0, "the very first track was refused: {body}");
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_key_queue_answers_overloaded() -> TestResult {
    let server = server_or_skip!();
    let admission = generous()?.with_key(KeyQueueDepth::try_from(1)?);
    let handle = start(&server, service_config("edge-a")?.with_admission(admission)).await?;
    let probe = Probe::new(&server).await;
    let codes = probe.burst("ana", 40).await?;
    assert!(count(&codes, "overloaded") > 0, "no command was shed: {codes:?}");
    assert!(count(&codes, "ok") > 0, "every command was shed: {codes:?}");
    assert!(handle.stats().overloaded() > 0);
    assert_eq!(handle.stats().rate_limited(), 0);
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_past_its_rate_is_refused() -> TestResult {
    let server = server_or_skip!();
    let admission =
        AdmissionLimits::default().with_key_rate(RateLimit::new(Duration::from_secs(60), RateBurst::try_from(5)?)?);
    let handle = start(&server, service_config("edge-a")?.with_admission(admission)).await?;
    let probe = Probe::new(&server).await;
    let holder = HolderId::generate()?;
    for index in 0..5 {
        probe.tracked("ana", &format!("room:r{index}"), holder).await?;
    }
    let (code, body) = probe.track("ana", "room:r5", holder).await?;
    assert_eq!(code, "overloaded", "{body}");
    assert_eq!(body["retryable"], true, "{body}");
    assert_eq!(handle.stats().rate_limited(), 1);
    probe.tracked("bob", "room:r5", holder).await?;
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_atomic_release_frees_a_holder_slot() -> TestResult {
    let server = server_or_skip!();
    let limits = ManagedLimits::default().with_holders(HolderLimit::try_from(2)?);
    let handle = start(&server, service_config("edge-a")?.with_managed_limits(limits)).await?;
    let probe = Probe::new(&server).await;
    let leaving = HolderId::generate()?;
    let staying = HolderId::generate()?;
    let waiting = HolderId::generate()?;
    let lobby = probe.tracked("ana", TOPIC, leaving).await?;
    let kitchen = probe.tracked("ana", "room:kitchen", leaving).await?;
    probe.tracked("ana", TOPIC, staying).await?;
    let (code, body) = probe.track("ana", TOPIC, waiting).await?;
    assert_eq!(code, "holder_limit", "{body}");

    let (code, body) = probe
        .holder_op(
            HolderOp::Release,
            "ana",
            json!({
                "holder": leaving,
                "targets": [
                    { "topic": TOPIC, "lifetime": lobby["lifetime"] },
                    { "topic": "room:kitchen", "lifetime": kitchen["lifetime"] },
                ],
            }),
        )
        .await?;
    assert_eq!(code, "ok", "{body}");
    assert_eq!(body["holder_freed"], true, "{body}");
    assert_eq!(body["released"][0]["status"], "released", "{body}");
    assert_eq!(body["released"][1]["status"], "released", "{body}");

    probe.tracked("ana", TOPIC, waiting).await?;
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_heartbeat_for_another_lifetime_is_gone() -> TestResult {
    let server = server_or_skip!();
    let handle = start(&server, service_config("edge-a")?).await?;
    let probe = Probe::new(&server).await;
    let holder = HolderId::generate()?;
    let lobby = probe.tracked("ana", TOPIC, holder).await?;
    let kitchen = probe.tracked("ana", "room:kitchen", holder).await?;

    let (code, body) = probe
        .holder_op(
            HolderOp::Heartbeat,
            "ana",
            json!({ "entries": [
                { "holder": holder, "topic": TOPIC, "lifetime": lobby["lifetime"], "mutation_seq": lobby["mutation_seq"] },
                { "holder": holder, "topic": TOPIC, "lifetime": kitchen["lifetime"], "mutation_seq": kitchen["mutation_seq"] },
            ] }),
        )
        .await?;
    assert_eq!(code, "ok", "{body}");
    assert_eq!(body["entries"], json!(["ok", "gone"]), "{body}");
    handle.shutdown().await;
    Ok(())
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum Delivery {
    Deliver = 0,
    LoseAck = 1,
    Drop = 2,
}

struct FlakySink {
    inner: NatsBatchSink,
    mode: Arc<AtomicU8>,
}

impl FlakySink {
    fn set(&self, delivery: Delivery) {
        self.mode.store(delivery as u8, Ordering::SeqCst);
    }
}

impl BatchSink for FlakySink {
    fn publish(&self, batch: AtomicBatch) -> SinkFuture<'_> {
        let mode = self.mode.load(Ordering::SeqCst);
        Box::pin(async move {
            if mode == Delivery::Drop as u8 {
                return Ok(BatchOutcome::Unknown);
            }
            let outcome = self.inner.publish(batch).await?;
            if mode == Delivery::LoseAck as u8 {
                return Ok(BatchOutcome::Unknown);
            }
            Ok(outcome)
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_batch_outcome_resolves_from_the_receipt() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = service_config("edge-a")?;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let presence = Presence::open(client.clone(), presence_config()?).await?;
    let key: PresenceKey = "ana".parse()?;
    let sink = Arc::new(FlakySink {
        inner: NatsBatchSink::new(client),
        mode: Arc::new(AtomicU8::new(Delivery::Deliver as u8)),
    });
    let mut coordinator = presence
        .coordinator(
            key.clone(),
            OwnerId::generate()?,
            OwnerDeadline::unbounded(SuspendAwareClock::system()?),
            ManagedLimits::default(),
        )?
        .with_sink(sink.clone());
    coordinator.ensure_ready().await?;
    let holder = HolderId::generate()?;
    let window = coordinator.retry_window();

    sink.set(Delivery::LoseAck);
    let landed = RequestIdentity::mint(window)?
        .request(holder)
        .track(TOPIC.parse()?, key.clone(), meta("online")?);
    let outcome = coordinator.submit(landed, None, None).await?;
    assert!(matches!(outcome, WriteOutcome::Applied(_)), "unexpected {outcome:?}");

    sink.set(Delivery::Drop);
    let lost =
        RequestIdentity::mint(window)?
            .request(holder)
            .track("room:kitchen".parse()?, key.clone(), meta("online")?);
    let err = coordinator
        .submit(lost.clone(), None, None)
        .await
        .err()
        .ok_or("a dropped batch reported success")?;
    assert!(matches!(err, ManagedError::OutcomeUnknown), "unexpected {err:?}");

    sink.set(Delivery::Deliver);
    let outcome = coordinator.submit(lost, None, None).await?;
    assert!(matches!(outcome, WriteOutcome::Applied(_)), "unexpected {outcome:?}");
    assert_eq!(coordinator.topic_count(&holder), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_inbox_outside_the_caller_prefix_is_dropped() -> TestResult {
    let server = server_or_skip!();
    let handle = start(&server, service_config("edge-a")?).await?;
    let probe = Probe::new(&server).await;
    let key: PresenceKey = "ana".parse()?;
    let other: PresenceKey = "bob".parse()?;
    let holder = HolderId::generate()?;
    let subject = WriteOp::Track.subject(&key, &TOPIC.parse()?);
    let body = json!({ "holder": holder, "meta": { "status": "online" } });

    let foreign = common::caller_inbox(&other)?;
    for inbox in [format!("_INBOX.{}.x", HolderId::generate()?), foreign] {
        let outcome = common::command_via(&probe.client, subject.clone(), inbox.clone(), &body).await;
        assert!(outcome.is_err(), "{inbox} got a reply");
    }
    assert_eq!(handle.stats().rejected_inbox(), 2);
    assert_eq!(handle.stats().forwarded(), 0);

    probe.tracked("ana", TOPIC, holder).await?;
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn internal_write_ignores_a_stale_owner_epoch() -> TestResult {
    let server = server_or_skip!();
    let config = service_config("edge-a")?;
    let first = start(&server, config.clone()).await?;
    let key: PresenceKey = "ana".parse()?;
    let topic: Topic = TOPIC.parse()?;
    let shards = ShardCount::DEFAULT;
    let probe = Probe::new(&server).await;
    let leases = LeaseStore::open(
        probe.client.clone(),
        config.lease_bucket().clone(),
        shards,
        config.lease_ttl(),
        LeaseValue::new(OwnerId::generate()?, StreamGeneration::generate()?),
    )
    .await?;
    let shard = LeaseKey::Writer(WriterShard::of(&key, shards));
    let stale = leases.holder(shard).await?.ok_or("the writer shard has no holder")?;
    first.shutdown().await;

    let second = start(&server, service_config("edge-b")?).await?;
    let current = leases.holder(shard).await?.ok_or("the writer shard has no holder")?;
    assert_ne!(stale.value().owner(), current.value().owner());

    let holder = HolderId::generate()?;
    let command = Command::Track {
        topic: topic.clone(),
        request: RequestIdentity::mint(RetryWindow::for_ttl(presence_config()?.lease_ttl()))?,
        holder,
        meta: meta("online")?,
        enriched: None,
    };
    let inbox = common::caller_inbox(&key)?;
    let envelope = Forwarded::new(
        key.clone(),
        OwnerEpoch::new(stale.revision(), stale.value().owner()),
        current.value().generation(),
        Some(inbox.clone().into()),
        command,
    );
    let reply = common::command_via(
        &probe.client,
        internal_write_subject(shards, &key, Some(&topic)),
        inbox,
        &serde_json::to_value(&envelope)?,
    )
    .await?;
    assert_eq!(
        common::code_of(&reply),
        Some("not_ready"),
        "{:?}",
        common::body_of(&reply)?
    );
    assert_eq!(second.stats().admitted(), 0);

    probe.tracked("ana", TOPIC, holder).await?;
    second.shutdown().await;
    Ok(())
}
