mod common;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use trogon_presence::{
    AtomicBatch, AuthorityId, BatchSink, ControlKey, EntryKey, EntryTarget, HeartbeatInterval, HolderId,
    KeyCoordinator, KvKey, LeaseTtl, ManagedLimits, MarkerTtl, Meta, NatsBatchSink, OwnerDeadline, OwnerEpoch, OwnerId,
    Presence, PresenceConfig, PresenceKey, ProvisionOptions, RetryWindow, ShardCount, SinkFuture, StreamGeneration,
    SuspendAwareClock, Topic, WriteIntent, WriteOutcome, WriteReceipt, WriteRequest, WriterMode, WriterShard,
};
use trogon_presence_service::subjects::internal_write_subject;
use trogon_presence_service::{
    Command, EntryRef, Forwarded, KeepaliveInterval, LeaseKey, LeaseStore, LeaseValue, NodeId, RequestIdentity,
    ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp, WriterReplyDeadline,
};

use common::{BoxError, NatsServer};

type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const OTHER: &str = "room:kitchen";
const LEASE_TTL: Duration = Duration::from_secs(30);
const HOLD_TIMEOUT: Duration = Duration::from_secs(10);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
const LANDING_TIMEOUT: Duration = Duration::from_secs(10);
const LANDING_POLL: Duration = Duration::from_millis(20);

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

fn meta(status: &str) -> Result<Meta, BoxError> {
    Ok(serde_json::from_value(json!({ "status": status }))?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Hold {
    Deliver = 0,
    BeforeCommit = 1,
    AfterCommit = 2,
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Track,
    Update,
    Untrack,
}

struct HoldingSink {
    inner: NatsBatchSink,
    mode: AtomicU8,
    reached: tokio::sync::Notify,
}

impl BatchSink for HoldingSink {
    fn publish(&self, batch: AtomicBatch) -> SinkFuture<'_> {
        let mode = self.mode.swap(Hold::Deliver as u8, Ordering::SeqCst);
        Box::pin(async move {
            if mode == Hold::BeforeCommit as u8 {
                self.reached.notify_one();
                return std::future::pending().await;
            }
            let outcome = self.inner.publish(batch).await;
            if mode == Hold::AfterCommit as u8 && outcome.is_ok() {
                self.reached.notify_one();
                return std::future::pending().await;
            }
            outcome
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Record {
    Absent,
    Live(u64),
    Purged,
}

struct Stored {
    stream: async_nats::jetstream::stream::Stream,
    config: PresenceConfig,
}

impl Stored {
    async fn record(&self, key: &KvKey) -> Result<Record, BoxError> {
        let subject = self.config.bucket().subject_for(key);
        match self.stream.get_last_raw_message_by_subject(&subject).await {
            Ok(message) => {
                let purged = message
                    .headers
                    .get("KV-Operation")
                    .is_some_and(|op| op.as_str() == "PURGE");
                Ok(if purged {
                    Record::Purged
                } else {
                    Record::Live(message.sequence)
                })
            }
            Err(err) if err.kind() == async_nats::jetstream::stream::LastRawMessageErrorKind::NoMessageFound => {
                Ok(Record::Absent)
            }
            Err(err) => Err(err.into()),
        }
    }
}

struct Scenario {
    coordinator: KeyCoordinator,
    sink: Arc<HoldingSink>,
    stored: Stored,
    key: PresenceKey,
    holder: HolderId,
}

impl Scenario {
    async fn open(server: &NatsServer) -> Result<Self, BoxError> {
        let client = server.client().await;
        trogon_presence_service::provision(client.clone(), &service_config("edge-a")?, ProvisionOptions::default())
            .await?;
        let config = presence_config()?;
        let presence = Presence::open(client.clone(), config.clone()).await?;
        let key: PresenceKey = "ana".parse()?;
        let sink = Arc::new(HoldingSink {
            inner: NatsBatchSink::new(client.clone()),
            mode: AtomicU8::new(Hold::Deliver as u8),
            reached: tokio::sync::Notify::new(),
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
        let stream = async_nats::jetstream::new(client)
            .get_stream(config.bucket().stream_name())
            .await?;
        Ok(Self {
            coordinator,
            sink,
            stored: Stored { stream, config },
            key,
            holder: HolderId::generate()?,
        })
    }

    fn request(&self) -> Result<WriteRequest, BoxError> {
        Ok(RequestIdentity::mint(self.coordinator.retry_window())?.request(self.holder))
    }

    fn track(&self, topic: &str) -> Result<WriteIntent, BoxError> {
        Ok(self.request()?.track(topic.parse()?, self.key.clone(), meta("online")?))
    }

    fn intent(&self, op: Op, existing: Option<&WriteReceipt>) -> Result<WriteIntent, BoxError> {
        let target = || -> Result<EntryTarget, BoxError> {
            let receipt = existing.ok_or("no existing entry to target")?;
            Ok(EntryTarget::new(receipt.lifetime(), receipt.sequence()))
        };
        let request = self.request()?;
        Ok(match op {
            Op::Track => request.track(TOPIC.parse()?, self.key.clone(), meta("online")?),
            Op::Update => request.update(TOPIC.parse()?, self.key.clone(), target()?, meta("away")?),
            Op::Untrack => request.untrack(TOPIC.parse()?, self.key.clone(), target()?),
        })
    }

    fn entry_key(&self, topic: &str) -> Result<KvKey, BoxError> {
        Ok(EntryKey::new(topic.parse::<Topic>()?, self.key.clone(), self.holder).encode(ShardCount::DEFAULT)?)
    }

    fn receipt_key(&self, intent: &WriteIntent) -> Result<KvKey, BoxError> {
        Ok(ControlKey::Receipt(AuthorityId::Managed(self.key.clone()), intent.request().operation_id()).encode()?)
    }

    async fn applied(&mut self, intent: WriteIntent) -> Result<WriteReceipt, BoxError> {
        match self.coordinator.submit(intent, None, None).await? {
            WriteOutcome::Applied(receipt) => Ok(receipt),
            outcome => Err(format!("expected an applied write, got {outcome:?}").into()),
        }
    }

    async fn abandon(&mut self, intent: WriteIntent, hold: Hold) -> TestResult {
        let sink = self.sink.clone();
        sink.mode.store(hold as u8, Ordering::SeqCst);
        tokio::select! {
            outcome = self.coordinator.submit(intent, None, None) => {
                Err(format!("the held write finished on its own: {outcome:?}").into())
            }
            reached = tokio::time::timeout(HOLD_TIMEOUT, sink.reached.notified()) => {
                reached.map_err(|_| "the write never reached the sink".into())
            }
        }
    }
}

async fn dropped_mid_commit(op: Op, hold: Hold) -> TestResult {
    let Some(server) = NatsServer::start().await else {
        return Ok(());
    };
    let mut scenario = Scenario::open(&server).await?;
    let existing = match op {
        Op::Track => None,
        Op::Update | Op::Untrack => Some(scenario.applied(scenario.track(TOPIC)?).await?),
    };
    let entry = scenario.entry_key(TOPIC)?;
    let before = scenario.stored.record(&entry).await?;
    let intent = scenario.intent(op, existing.as_ref())?;
    let receipt_key = scenario.receipt_key(&intent)?;

    scenario.abandon(intent.clone(), hold).await?;

    let receipt = scenario.stored.record(&receipt_key).await?;
    let after = scenario.stored.record(&entry).await?;
    match hold {
        Hold::BeforeCommit => {
            assert_eq!(
                receipt,
                Record::Absent,
                "{op:?}: a write held before commit left a receipt"
            );
            assert_eq!(after, before, "{op:?}: a write held before commit changed the entry");
        }
        Hold::AfterCommit | Hold::Deliver => {
            assert!(
                matches!(receipt, Record::Live(_)),
                "{op:?}: a committed write has no receipt"
            );
            match op {
                Op::Track | Op::Update => assert!(
                    matches!(after, Record::Live(_)) && after != before,
                    "{op:?}: a committed write left the entry at {after:?}, it was {before:?}"
                ),
                Op::Untrack => assert_eq!(after, Record::Purged, "{op:?}: a committed untrack left the entry"),
            }
        }
    }

    let retried = scenario.coordinator.submit(intent, None, None).await?;
    match (hold, &retried) {
        (Hold::BeforeCommit, WriteOutcome::Applied(_)) => {}
        (Hold::AfterCommit | Hold::Deliver, WriteOutcome::Replayed { .. }) => {}
        _ => return Err(format!("{op:?} {hold:?}: unexpected retry outcome {retried:?}").into()),
    }
    let settled = scenario.stored.record(&entry).await?;
    let expected_topics = match op {
        Op::Untrack => {
            assert_eq!(
                settled,
                Record::Purged,
                "{op:?}: the retry left the entry at {settled:?}"
            );
            0
        }
        Op::Track | Op::Update => {
            assert!(
                matches!(settled, Record::Live(_)),
                "{op:?}: the retry left the entry at {settled:?}"
            );
            1
        }
    };
    assert_eq!(
        scenario.coordinator.topic_count(&scenario.holder),
        expected_topics,
        "{op:?} {hold:?}: the guard index disagrees with the stored entries after the retry"
    );

    let outcome = scenario.coordinator.submit(scenario.track(OTHER)?, None, None).await;
    assert!(
        matches!(outcome, Ok(WriteOutcome::Applied(_))),
        "{op:?} {hold:?}: the next mutation after an abandoned commit failed: {outcome:?}"
    );
    assert_eq!(
        scenario.coordinator.topic_count(&scenario.holder),
        expected_topics + 1,
        "{op:?} {hold:?}: the guard index lost a slot"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_track_dropped_before_commit_leaves_nothing_behind() -> TestResult {
    dropped_mid_commit(Op::Track, Hold::BeforeCommit).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_track_dropped_after_commit_keeps_the_key_writable() -> TestResult {
    dropped_mid_commit(Op::Track, Hold::AfterCommit).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_dropped_before_commit_leaves_nothing_behind() -> TestResult {
    dropped_mid_commit(Op::Update, Hold::BeforeCommit).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_dropped_after_commit_keeps_the_key_writable() -> TestResult {
    dropped_mid_commit(Op::Update, Hold::AfterCommit).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untrack_dropped_before_commit_leaves_nothing_behind() -> TestResult {
    dropped_mid_commit(Op::Untrack, Hold::BeforeCommit).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untrack_dropped_after_commit_keeps_the_key_writable() -> TestResult {
    dropped_mid_commit(Op::Untrack, Hold::AfterCommit).await
}

struct Ingress<'a> {
    server: &'a NatsServer,
    client: async_nats::Client,
    stored: Stored,
    key: PresenceKey,
    holder: HolderId,
}

impl<'a> Ingress<'a> {
    async fn open(server: &'a NatsServer) -> Result<Self, BoxError> {
        let client = server.client().await;
        let config = presence_config()?;
        trogon_presence_service::provision(client.clone(), &service_config("edge-a")?, ProvisionOptions::default())
            .await?;
        let stream = async_nats::jetstream::new(client.clone())
            .get_stream(config.bucket().stream_name())
            .await?;
        Ok(Self {
            server,
            client,
            stored: Stored { stream, config },
            key: "ana".parse()?,
            holder: HolderId::generate()?,
        })
    }

    async fn start(&self, config: ServiceConfig) -> Result<ServiceHandle, BoxError> {
        let handle = trogon_presence_service::start(self.server.client().await, config).await?;
        common::wait_for_writers(&handle, usize::from(ShardCount::DEFAULT.get()), OWNERSHIP_TIMEOUT).await?;
        Ok(handle)
    }

    async fn send(&self, op: WriteOp, topic: &str, body: &Value) -> Result<(String, Value), BoxError> {
        let reply = common::command(&self.client, op.subject(&self.key, &topic.parse()?), &self.key, body).await?;
        let code = common::code_of(&reply).ok_or("reply has no code")?.to_owned();
        Ok((code, common::body_of(&reply)?))
    }

    fn body(&self, op: Op, request: RequestIdentity, existing: Option<&Value>) -> Result<Value, BoxError> {
        let mut body = json!({ "holder": self.holder, "request": request });
        if let Op::Track | Op::Update = op {
            body["meta"] = json!({ "status": "online" });
        }
        if let Op::Update | Op::Untrack = op {
            let existing = existing.ok_or("no existing entry to target")?;
            body["lifetime"] = existing["lifetime"].clone();
            body["mutation_seq"] = existing["mutation_seq"].clone();
        }
        Ok(body)
    }

    async fn forward(
        &self,
        config: &ServiceConfig,
        op: Op,
        request: RequestIdentity,
        existing: Option<&Value>,
    ) -> Result<(String, Value), BoxError> {
        let shards = ShardCount::DEFAULT;
        let leases = LeaseStore::open(
            self.client.clone(),
            config.lease_bucket().clone(),
            shards,
            config.lease_ttl(),
            LeaseValue::new(OwnerId::generate()?, StreamGeneration::generate()?),
        )
        .await?;
        let writer = leases
            .holder(LeaseKey::Writer(WriterShard::of(&self.key, shards)))
            .await?
            .ok_or("the writer shard has no holder")?;
        let topic: Topic = TOPIC.parse()?;
        let target = || -> Result<EntryRef, BoxError> {
            let existing = existing.ok_or("no existing entry to target")?;
            Ok(EntryRef::new(
                serde_json::from_value(existing["lifetime"].clone())?,
                serde_json::from_value(existing["mutation_seq"].clone())?,
            ))
        };
        let command = match op {
            Op::Track => Command::Track {
                topic: topic.clone(),
                request,
                holder: self.holder,
                meta: meta("online")?,
                enriched: None,
            },
            Op::Update => Command::Update {
                topic: topic.clone(),
                request,
                holder: self.holder,
                meta: meta("online")?,
                enriched: None,
                target: target()?,
                expected_ref: None,
            },
            Op::Untrack => Command::Untrack {
                topic: topic.clone(),
                request,
                holder: self.holder,
                target: target()?,
            },
        };
        let inbox = common::caller_inbox(&self.key)?;
        let envelope = Forwarded::new(
            self.key.clone(),
            OwnerEpoch::new(writer.revision(), writer.value().owner()),
            writer.value().generation(),
            Some(inbox.clone().into()),
            command,
        );
        let reply = common::command_via(
            &self.client,
            internal_write_subject(shards, &self.key, Some(&topic)),
            inbox,
            &serde_json::to_value(&envelope)?,
        )
        .await?;
        let code = common::code_of(&reply).ok_or("reply has no code")?.to_owned();
        Ok((code, common::body_of(&reply)?))
    }

    async fn stored_entry(&self) -> Result<Option<Value>, BoxError> {
        let entry =
            EntryKey::new(TOPIC.parse::<Topic>()?, self.key.clone(), self.holder).encode(ShardCount::DEFAULT)?;
        Ok(match self.stored.record(&entry).await? {
            Record::Live(_) => {
                let subject = self.stored.config.bucket().subject_for(&entry);
                let message = self.stored.stream.get_last_raw_message_by_subject(&subject).await?;
                Some(serde_json::from_slice(&message.payload)?)
            }
            Record::Absent | Record::Purged => None,
        })
    }

    async fn landed(&self, request: RequestIdentity) -> TestResult {
        let operation = request.request(self.holder).operation_id();
        let receipt = ControlKey::Receipt(AuthorityId::Managed(self.key.clone()), operation).encode()?;
        tokio::time::timeout(LANDING_TIMEOUT, async {
            while self.stored.record(&receipt).await? == Record::Absent {
                tokio::time::sleep(LANDING_POLL).await;
            }
            Ok::<_, BoxError>(())
        })
        .await
        .map_err(|_| "the abandoned write never landed")?
    }
}

impl Op {
    fn wire(self) -> WriteOp {
        match self {
            Op::Track => WriteOp::Track,
            Op::Update => WriteOp::Update,
            Op::Untrack => WriteOp::Untrack,
        }
    }
}

async fn abandoned_at_the_ingress(op: Op) -> TestResult {
    let Some(server) = NatsServer::start().await else {
        return Ok(());
    };
    let ingress = Ingress::open(&server).await?;
    let window = RetryWindow::for_ttl(presence_config()?.lease_ttl());

    let patient = ingress.start(service_config("edge-a")?).await?;
    let existing = match op {
        Op::Track => None,
        Op::Update | Op::Untrack => {
            let body = ingress.body(Op::Track, RequestIdentity::mint(window)?, None)?;
            let (code, reply) = ingress.send(WriteOp::Track, TOPIC, &body).await?;
            assert_eq!(code, "ok", "{reply}");
            Some(reply)
        }
    };
    patient.shutdown().await;

    let request = RequestIdentity::mint(window)?;
    let body = ingress.body(op, request, existing.as_ref())?;
    let hasty =
        service_config("edge-b")?.with_writer_reply_deadline(WriterReplyDeadline::try_from(Duration::from_millis(1))?);
    let impatient = ingress.start(hasty.clone()).await?;
    let (code, reply) = ingress.forward(&hasty, op, request, existing.as_ref()).await?;
    assert_eq!(code, "unavailable", "{op:?}: {reply}");
    assert_eq!(reply["outcome_unknown"], true, "{op:?}: {reply}");
    ingress.landed(request).await?;
    impatient.shutdown().await;

    let recovered = ingress.start(service_config("edge-c")?).await?;
    let (code, retried) = ingress.send(op.wire(), TOPIC, &body).await?;
    assert_eq!(code, "ok", "{op:?}: the retry of an unknown outcome failed: {retried}");
    let stored = ingress.stored_entry().await?;
    match op {
        Op::Untrack => {
            assert_eq!(retried["untracked"], true, "{retried}");
            assert!(stored.is_none(), "{op:?}: the entry survived its untrack: {stored:?}");
        }
        Op::Track | Op::Update => {
            let stored = stored.ok_or("the entry is missing after its retry")?;
            assert_eq!(stored["lifetime"], retried["lifetime"], "{op:?}: {stored} vs {retried}");
            assert_eq!(
                stored["mutation_seq"], retried["mutation_seq"],
                "{op:?}: {stored} vs {retried}"
            );
            if let Some(existing) = &existing {
                assert_eq!(
                    retried["lifetime"], existing["lifetime"],
                    "{op:?}: the update replaced the lifetime"
                );
            }
            let follow = ingress.body(Op::Update, RequestIdentity::mint(window)?, Some(&retried))?;
            let (code, reply) = ingress.send(WriteOp::Update, TOPIC, &follow).await?;
            assert_eq!(
                code, "ok",
                "{op:?}: the follow up update after the retry failed: {reply}"
            );
        }
    }
    let other = json!({ "holder": ingress.holder, "meta": { "status": "online" } });
    let (code, reply) = ingress.send(WriteOp::Track, OTHER, &other).await?;
    assert_eq!(code, "ok", "{op:?}: {reply}");
    recovered.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_track_abandoned_at_the_ingress_resolves_on_retry() -> TestResult {
    abandoned_at_the_ingress(Op::Track).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_abandoned_at_the_ingress_resolves_on_retry() -> TestResult {
    abandoned_at_the_ingress(Op::Update).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untrack_abandoned_at_the_ingress_resolves_on_retry() -> TestResult {
    abandoned_at_the_ingress(Op::Untrack).await
}
