mod common;

use std::collections::BTreeSet;
use std::error::Error;
use std::time::Duration;

use async_nats::{HeaderMap, Message, Subscriber};
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::watch::{CursorStep, ViewCursor};
use trogon_presence::{
    ConnectionId, DiffSequence, EntryRevision, GenerationEpoch, HolderId, OwnerEpoch, OwnerId, Presence,
    PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, StreamGeneration, Topic, UnixMillis, ViewShard,
    WriterMode,
};
use trogon_presence_service::reply::{
    HEADER_CODE, HEADER_GENERATION, HEADER_KIND, HEADER_OWNER_ID, HEADER_OWNER_REV, HEADER_PREV, HEADER_SEQ,
    HEADER_SHARD, HEADER_TOPIC,
};
use trogon_presence_service::subjects::epoch_subject;
use trogon_presence_service::{
    KeepaliveInterval, NodeId, PayloadBudget, PresenceReader, ReadOp, ReaderIdentity, ReaderOptions, ServiceConfig,
    ServiceHandle, ShardLeaseTtl, SnapshotLimits, WriteOp,
};

use common::NatsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const TOPIC: &str = "room:lobby";
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn presence_config() -> PresenceConfig {
    PresenceConfig::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )
    .unwrap_or_else(|err| panic!("default presence config: {err}"))
    .with_writer_mode(WriterMode::Managed)
}

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    Ok(ServiceConfig::new(presence_config(), node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

async fn start_owning_all(server: &NatsServer, config: ServiceConfig) -> Result<ServiceHandle, BoxError> {
    let client = server.client().await;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    wait_for_owned(&handle, usize::from(ShardCount::DEFAULT.get())).await?;
    common::wait_for_writers(&handle, usize::from(ShardCount::DEFAULT.get()), OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

async fn wait_for_owned(handle: &ServiceHandle, expected: usize) -> Result<(), BoxError> {
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != expected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| format!("owned {} shards, expected {expected}", handle.owned_shards().len()).into())
}

fn header<'a>(headers: Option<&'a HeaderMap>, name: &str) -> Option<&'a str> {
    headers
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn rev(message: &Message, name: &str) -> Result<u64, BoxError> {
    Ok(header(message.headers.as_ref(), name)
        .ok_or_else(|| format!("missing {name}"))?
        .parse()?)
}

async fn next_frame(frames: &mut Subscriber, kind: &str) -> Result<Message, BoxError> {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let message = frames.next().await.ok_or("frame subscription ended")?;
            if header(message.headers.as_ref(), HEADER_KIND) == Some(kind) {
                return Ok::<_, BoxError>(message);
            }
        }
    })
    .await
    .map_err(|_| format!("no {kind} frame within {FRAME_TIMEOUT:?}"))?
}

async fn track_via_service(
    client: &async_nats::Client,
    topic: &Topic,
    key: &str,
    holder: &HolderId,
    status: &str,
) -> Result<Value, BoxError> {
    let key: PresenceKey = key.parse()?;
    let reply = common::command(
        client,
        WriteOp::Track.subject(&key, topic),
        &key,
        &json!({ "holder": holder, "meta": { "status": status } }),
    )
    .await?;
    if common::code_of(&reply) != Some("ok") {
        return Err(format!("track {key} replied {:?}", common::code_of(&reply)).into());
    }
    common::body_of(&reply)
}

async fn update_via_service(
    client: &async_nats::Client,
    topic: &Topic,
    key: &str,
    holder: &HolderId,
    tracked: &Value,
    status: &str,
) -> Result<Value, BoxError> {
    let key: PresenceKey = key.parse()?;
    let reply = common::command(
        client,
        WriteOp::Update.subject(&key, topic),
        &key,
        &json!({
            "holder": holder,
            "meta": { "status": status },
            "lifetime": tracked["lifetime"],
            "mutation_seq": tracked["mutation_seq"],
        }),
    )
    .await?;
    if common::code_of(&reply) != Some("ok") {
        return Err(format!("update {key} replied {:?}", common::code_of(&reply)).into());
    }
    common::body_of(&reply)
}

fn keys(body: &Value, side: &str) -> BTreeSet<String> {
    body[side]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn epoch_of(message: &Message) -> Result<GenerationEpoch, BoxError> {
    let headers = message.headers.as_ref();
    let generation: StreamGeneration = header(headers, HEADER_GENERATION)
        .ok_or("missing generation")?
        .parse()?;
    let owner: OwnerId = header(headers, HEADER_OWNER_ID).ok_or("missing owner id")?.parse()?;
    let acquired = EntryRevision::from(rev(message, HEADER_OWNER_REV)?);
    Ok(GenerationEpoch::new(generation, OwnerEpoch::new(acquired, owner)))
}

fn cursor_of(message: &Message) -> Result<ViewCursor, BoxError> {
    Ok(ViewCursor::Service {
        epoch: epoch_of(message)?,
        seq: DiffSequence::from(rev(message, HEADER_SEQ)?),
    })
}

struct Chain {
    epoch: Option<GenerationEpoch>,
    last_seq: u64,
}

impl Chain {
    fn new() -> Self {
        Self {
            epoch: None,
            last_seq: DiffSequence::FIRST.get(),
        }
    }

    fn accept(&mut self, message: &Message, shard: &str) -> Result<Value, BoxError> {
        let headers = message.headers.as_ref();
        let epoch = epoch_of(message)?;
        if let Some(previous) = &self.epoch {
            assert_eq!(previous, &epoch, "epoch changed without an ownership change");
        }
        self.epoch = Some(epoch);
        assert_eq!(header(headers, HEADER_SHARD), Some(shard));
        assert_eq!(header(headers, HEADER_TOPIC), Some(TOPIC));
        let prev = rev(message, HEADER_PREV)?;
        let current = rev(message, HEADER_SEQ)?;
        assert_eq!(
            prev, self.last_seq,
            "Presence-Prev must chain to the previous Presence-Seq"
        );
        assert_eq!(current, prev + 1, "Presence-Seq must step by one per diff");
        self.last_seq = current;
        Ok(serde_json::from_slice(&message.payload)?)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn broadcasts_diffs_and_answers_requests() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, service_config("edge-a")?).await?;
    let shards = ShardCount::DEFAULT;
    let topic: Topic = TOPIC.parse()?;
    let shard = shards.token(ViewShard::of(&topic, shards));

    let client = server.client().await;
    let mut frames = client
        .subscribe(trogon_presence_service::subjects::diff_subject(&topic))
        .await?;
    client.flush().await?;

    let ana = HolderId::generate()?;
    let ana_entry = track_via_service(&client, &topic, "ana", &ana, "online").await?;
    track_via_service(&client, &topic, "bob", &HolderId::generate()?, "away").await?;

    let mut chain = Chain::new();
    let mut joined = BTreeSet::new();
    while joined.len() < 2 {
        let body = chain.accept(&next_frame(&mut frames, "diff").await?, &shard)?;
        joined.extend(keys(&body, "joins"));
    }
    assert_eq!(joined, BTreeSet::from(["ana".to_owned(), "bob".to_owned()]));

    let caller: PresenceKey = "observer".parse()?;
    let listed = client
        .request(ReadOp::List.subject(shards, &caller, &topic), Vec::new().into())
        .await?;
    assert_eq!(header(listed.headers.as_ref(), HEADER_CODE), Some("ok"));
    assert_eq!(rev(&listed, HEADER_SEQ)?, chain.last_seq);
    assert!(header(listed.headers.as_ref(), HEADER_PREV).is_none());
    assert_eq!(Some(epoch_of(&listed)?), chain.epoch);
    let state: Value = serde_json::from_slice(&listed.payload)?;
    let metas: usize = ["ana", "bob"]
        .iter()
        .map(|key| state[key]["metas"].as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(metas, 2, "{state}");

    let got = client
        .request(
            ReadOp::Get.subject(shards, &caller, &topic),
            serde_json::to_vec(&json!({ "key": "ana" }))?.into(),
        )
        .await?;
    let got: Value = serde_json::from_slice(&got.payload)?;
    let metas = got["metas"].as_array().ok_or("get reply has no metas")?;
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0]["status"], "online");

    let redirected = client
        .request(
            format!("presence.v1.list._.observer.{}", topic.tokens()),
            Vec::new().into(),
        )
        .await?;
    let redirected_body: Value = serde_json::from_slice(&redirected.payload)?;
    assert_eq!(redirected_body["error"], "not_owner");
    assert_eq!(redirected_body["shard"], shard.as_str());

    let carol: PresenceKey = "carol".parse()?;
    let tracked = common::command(
        &client,
        WriteOp::Track.subject(&carol, &topic),
        &carol,
        &json!({ "holder": HolderId::generate()?, "meta": { "status": "busy" } }),
    )
    .await?;
    assert_eq!(header(tracked.headers.as_ref(), HEADER_CODE), Some("ok"));
    let tracked: Value = serde_json::from_slice(&tracked.payload)?;
    assert!(tracked["phx_ref"].is_string(), "{tracked}");
    let body = chain.accept(&next_frame(&mut frames, "diff").await?, &shard)?;
    assert_eq!(keys(&body, "joins"), BTreeSet::from(["carol".to_owned()]));

    let ana_key: PresenceKey = "ana".parse()?;
    let untracked = common::command(
        &client,
        WriteOp::Untrack.subject(&ana_key, &topic),
        &ana_key,
        &json!({ "holder": ana, "lifetime": ana_entry["lifetime"], "mutation_seq": ana_entry["mutation_seq"] }),
    )
    .await?;
    assert_eq!(common::code_of(&untracked), Some("ok"));
    let body = chain.accept(&next_frame(&mut frames, "diff").await?, &shard)?;
    assert_eq!(keys(&body, "leaves"), BTreeSet::from(["ana".to_owned()]));
    assert!(keys(&body, "joins").is_empty());

    let keepalive = next_frame(&mut frames, "keepalive").await?;
    assert_eq!(rev(&keepalive, HEADER_SEQ)?, chain.last_seq);
    assert_eq!(rev(&keepalive, HEADER_PREV)?, chain.last_seq);
    assert_eq!(Some(epoch_of(&keepalive)?), chain.epoch);

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn second_instance_takes_shards_after_release() -> TestResult {
    let server = server_or_skip!();
    let first = start_owning_all(&server, service_config("edge-a")?).await?;
    let second = trogon_presence_service::start(server.client().await, service_config("edge-b")?).await?;

    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        second.owned_shards().is_empty(),
        "second instance took shards held by the first"
    );
    assert_eq!(first.owned_shards().len(), usize::from(ShardCount::DEFAULT.get()));

    first.shutdown().await;
    wait_for_owned(&second, usize::from(ShardCount::DEFAULT.get())).await?;
    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn large_lists_are_refused_and_served_by_snapshots() -> TestResult {
    let server = server_or_skip!();
    let limits = SnapshotLimits::default().with_payload(PayloadBudget::try_from(1024)?);
    let config = service_config("edge-a")?.with_snapshot_limits(limits);
    let handle = start_owning_all(&server, config).await?;
    let shards = ShardCount::DEFAULT;
    let topic: Topic = TOPIC.parse()?;

    let client = server.client().await;
    let expected: BTreeSet<String> = (0..24).map(|index| format!("member-{index}")).collect();
    for key in &expected {
        track_via_service(&client, &topic, key, &HolderId::generate()?, "online").await?;
    }

    let caller: PresenceKey = "observer".parse()?;
    let refused = tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let reply = client
                .request(ReadOp::List.subject(shards, &caller, &topic), Vec::new().into())
                .await?;
            if error_of(&reply)? == "snapshot_too_large" {
                return Ok::<_, BoxError>(reply);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "list never exceeded the payload budget")??;
    assert_eq!(error_of(&refused)?, "snapshot_too_large");

    let reader = PresenceReader::start(
        client.clone(),
        ReaderIdentity::new(caller, ConnectionId::generate()?),
        topic.clone(),
        ReaderOptions::default().with_limits(limits),
    )
    .await?;
    let mut state = reader.watch();
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let listed: BTreeSet<String> = state.borrow().iter().map(|(key, _)| key.to_string()).collect();
            if listed == expected {
                return Ok::<_, BoxError>(());
            }
            state.changed().await?;
        }
    })
    .await
    .map_err(|_| "reader never assembled the full state")??;

    reader.close().await;
    handle.shutdown().await;
    Ok(())
}

async fn list_with(client: &async_nats::Client, topic: &Topic, body: Value) -> Result<Message, BoxError> {
    let caller: PresenceKey = "observer".parse()?;
    Ok(client
        .request(
            ReadOp::List.subject(ShardCount::DEFAULT, &caller, topic),
            serde_json::to_vec(&body)?.into(),
        )
        .await?)
}

fn error_of(message: &Message) -> Result<String, BoxError> {
    let body: Value = serde_json::from_slice(&message.payload)?;
    Ok(body["error"].as_str().unwrap_or("ok").to_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn epoch_announcement_and_lease_carry_generation_and_owner() -> TestResult {
    let server = server_or_skip!();
    let shards = ShardCount::DEFAULT;
    let topic: Topic = TOPIC.parse()?;
    let shard = ViewShard::of(&topic, shards);
    let client = server.client().await;
    let mut epochs = client.subscribe(epoch_subject(shards, shard)).await?;
    client.flush().await?;
    let config = service_config("edge-a")?;
    let handle = start_owning_all(&server, config.clone()).await?;
    let presence = Presence::open(server.client().await, presence_config()).await?;

    let announced = tokio::time::timeout(FRAME_TIMEOUT, epochs.next())
        .await?
        .ok_or("epoch subscription ended")?;
    let body: Value = serde_json::from_slice(&announced.payload)?;
    let epoch = epoch_of(&announced)?;
    assert_eq!(epoch.generation(), presence.generation());
    assert_eq!(body["generation"], presence.generation().to_string());
    assert_eq!(body["owner_epoch"]["owner"], epoch.epoch().owner().to_string());
    assert_eq!(body["owner_epoch"]["acquired"], epoch.epoch().acquired().to_string());
    assert_eq!(
        header(announced.headers.as_ref(), HEADER_SHARD),
        Some(shards.token(shard).as_str())
    );

    let leases = async_nats::jetstream::new(client.clone())
        .get_key_value(config.lease_bucket().as_str())
        .await?;
    let lease = leases
        .get(format!("view.{}", shards.token(shard)))
        .await?
        .ok_or("view lease is missing")?;
    let lease: Value = serde_json::from_slice(&lease)?;
    assert_eq!(lease["owner"], epoch.epoch().owner().to_string());
    assert_eq!(lease["generation"], presence.generation().to_string());
    assert!(lease.get("node").is_none(), "{lease}");

    presence.close();
    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_sequence_restarts_on_a_new_owner_epoch() -> TestResult {
    let server = server_or_skip!();
    let shards = ShardCount::DEFAULT;
    let topic: Topic = TOPIC.parse()?;
    let shard = shards.token(ViewShard::of(&topic, shards));
    let client = server.client().await;
    let mut frames = client
        .subscribe(trogon_presence_service::subjects::diff_subject(&topic))
        .await?;
    client.flush().await?;
    let first = start_owning_all(&server, service_config("edge-a")?).await?;
    let ana = HolderId::generate()?;
    let tracked = track_via_service(&client, &topic, "ana", &ana, "online").await?;
    let mut chain = Chain::new();
    let joined = next_frame(&mut frames, "diff").await?;
    chain.accept(&joined, &shard)?;
    let reader = cursor_of(&joined)?;
    assert_eq!(reader.seq(), DiffSequence::from(2));

    first.shutdown().await;
    let second = start_owning_all(&server, service_config("edge-b")?).await?;
    let announced = tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let keepalive = next_frame(&mut frames, "keepalive").await?;
            if epoch_of(&keepalive)? != epoch_of(&joined)? {
                return Ok::<_, BoxError>(keepalive);
            }
        }
    })
    .await
    .map_err(|_| "no keepalive from the new owner")??;
    let rebased = cursor_of(&announced)?;
    assert_eq!(rebased.seq(), DiffSequence::FIRST);
    assert_eq!(rev(&announced, HEADER_PREV)?, DiffSequence::FIRST.get());
    assert_eq!(reader.follow(&rebased, DiffSequence::FIRST), CursorStep::Rebase);

    track_via_service(&client, &topic, "bob", &HolderId::generate()?, "away").await?;
    let missed = next_frame(&mut frames, "diff").await?;
    assert_eq!(rev(&missed, HEADER_SEQ)?, 2);
    update_via_service(&client, &topic, "ana", &ana, &tracked, "busy").await?;
    let after = next_frame(&mut frames, "diff").await?;
    let prev = DiffSequence::from(rev(&after, HEADER_PREV)?);
    assert_eq!(rebased.follow(&cursor_of(&after)?, prev), CursorStep::Gap);
    assert_eq!(cursor_of(&missed)?.follow(&cursor_of(&after)?, prev), CursorStep::Next);

    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn list_waits_for_a_per_entry_barrier() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, service_config("edge-a")?).await?;
    let topic: Topic = TOPIC.parse()?;
    let client = server.client().await;
    let presence = Presence::open(server.client().await, presence_config()).await?;
    let ana = HolderId::generate()?;
    let tracked = track_via_service(&client, &topic, "ana", &ana, "online").await?;
    let revision: EntryRevision = serde_json::from_value(tracked["rev"].clone())?;
    let barrier = |target: EntryRevision, generation: StreamGeneration, window: u64| {
        json!({ "barrier": {
            "generation": generation,
            "key": "ana",
            "holder": ana,
            "target": target,
            "expires": UnixMillis::now().saturating_add(Duration::from_millis(window)),
        }})
    };

    let listed = list_with(&client, &topic, barrier(revision, presence.generation(), 5_000)).await?;
    assert_eq!(header(listed.headers.as_ref(), HEADER_CODE), Some("ok"));
    let state: Value = serde_json::from_slice(&listed.payload)?;
    assert!(state["ana"]["metas"].is_array(), "{state}");

    let far = EntryRevision::from(revision.get() + 1_000);
    let expired = list_with(&client, &topic, barrier(far, presence.generation(), 5_000)).await?;
    assert_eq!(error_of(&expired)?, "barrier_expired");

    let foreign = StreamGeneration::from([9; 16]);
    let refused = list_with(&client, &topic, barrier(revision, foreign, 5_000)).await?;
    assert_eq!(error_of(&refused)?, "generation_changed");

    presence.close();
    handle.shutdown().await;
    Ok(())
}
