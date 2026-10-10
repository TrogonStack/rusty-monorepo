mod common;

use std::collections::BTreeSet;
use std::error::Error;
use std::time::Duration;

use async_nats::header::NATS_MESSAGE_TTL;
use async_nats::jetstream::{self, stream::Stream};
use async_nats::HeaderMap;
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::value::StoredValue;
use trogon_presence::watch::replay::{RebuildBudget, ReconcileInterval};
use trogon_presence::{
    EntryKey, HolderId, KvKey, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic, ViewShard, WriterMode,
};
use trogon_presence_service::reply::HEADER_CODE;
use trogon_presence_service::{
    KeepaliveInterval, NodeId, ReadOp, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
};

use common::NatsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const TOPIC: &str = "room:lobby";
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(10);
const LONG_TTL: &str = "300s";

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn presence_config() -> PresenceConfig {
    PresenceConfig::default().with_writer_mode(WriterMode::Managed)
}

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    Ok(ServiceConfig::new(presence_config(), node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

async fn track(client: &async_nats::Client, topic: &Topic, key: &str, status: &str) -> Result<(), BoxError> {
    let key: PresenceKey = key.parse()?;
    let reply = common::command(
        client,
        WriteOp::Track.subject(&key, topic),
        &key,
        &json!({ "holder": HolderId::generate()?, "meta": { "status": status } }),
    )
    .await?;
    match common::code_of(&reply) {
        Some("ok") => Ok(()),
        code => Err(format!("track {key} replied {code:?}").into()),
    }
}

fn stored(topic: &Topic, key: &PresenceKey) -> Result<Vec<u8>, BoxError> {
    let json = json!({
        "v": 2,
        "topic": topic,
        "key": key,
        "phx_ref": "Fq1",
        "phx_ref_prev": null,
        "meta": { "status": "online" },
        "lifetime": "AAAAAAAAAAAAAAAAAAAAAA",
        "mutation_seq": "1",
        "last_op": { "id": "AQEBAQEBAQEBAQEBAQEBAQ", "fingerprint": "A".repeat(43), "outcome": "tracked" },
        "client_meta": { "status": "online" },
        "birth_rev": null,
    });
    Ok(StoredValue::from_json_bytes(&serde_json::to_vec(&json)?)?.to_json_bytes()?)
}

async fn publish(client: &async_nats::Client, kv_key: &KvKey, payload: Vec<u8>) -> Result<(), BoxError> {
    let mut headers = HeaderMap::new();
    headers.insert(NATS_MESSAGE_TTL, LONG_TTL);
    jetstream::new(client.clone())
        .publish_with_headers(presence_config().bucket().subject_for(kv_key), headers, payload.into())
        .await?
        .await?;
    Ok(())
}

async fn presence_stream(client: &async_nats::Client) -> Result<Stream, BoxError> {
    Ok(jetstream::new(client.clone())
        .get_stream(presence_config().bucket().stream_name())
        .await?)
}

async fn start_owning_all(server: &NatsServer, config: ServiceConfig) -> Result<ServiceHandle, BoxError> {
    let client = server.client().await;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    let expected = usize::from(ShardCount::DEFAULT.get());
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != expected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "service did not own every shard")?;
    Ok(handle)
}

async fn listed_keys(client: &async_nats::Client, topic: &Topic) -> Result<Option<BTreeSet<String>>, BoxError> {
    let caller: PresenceKey = "observer".parse()?;
    let reply = client
        .request(
            ReadOp::List.subject(ShardCount::DEFAULT, &caller, topic),
            Vec::new().into(),
        )
        .await?;
    let code = reply
        .headers
        .as_ref()
        .and_then(|headers| headers.get(HEADER_CODE))
        .map(|value| value.as_str().to_owned());
    if code.as_deref() != Some("ok") {
        return Ok(None);
    }
    let state: Value = serde_json::from_slice(&reply.payload)?;
    Ok(Some(
        state
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default(),
    ))
}

async fn wait_for_listing(
    client: &async_nats::Client,
    topic: &Topic,
    expected: &BTreeSet<String>,
) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + CONVERGE_TIMEOUT;
    loop {
        let listed = listed_keys(client, topic).await?;
        if listed.as_ref() == Some(expected) {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            return Err(format!("listing {listed:?} never became {expected:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn keys<const N: usize>(names: [&str; N]) -> BTreeSet<String> {
    names.into_iter().map(str::to_owned).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn shard_owner_rebuilds_after_its_consumer_is_deleted() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, service_config("edge-a")?).await?;
    let client = server.client().await;
    let topic: Topic = TOPIC.parse()?;
    track(&client, &topic, "ana", "online").await?;
    wait_for_listing(&client, &topic, &keys(["ana"])).await?;

    let stream = presence_stream(&client).await?;
    let mut names = Vec::new();
    let mut listing = stream.consumer_names();
    while let Some(name) = listing.next().await {
        names.push(name?);
    }
    assert!(!names.is_empty());
    for name in &names {
        stream.delete_consumer(name).await?;
    }

    track(&client, &topic, "bob", "away").await?;
    wait_for_listing(&client, &topic, &keys(["ana", "bob"])).await?;

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shard_owner_reconcile_repairs_an_injected_divergence() -> TestResult {
    let server = server_or_skip!();
    let config = service_config("edge-a")?.with_reconcile(ReconcileInterval::try_from(Duration::from_secs(1))?);
    let handle = start_owning_all(&server, config).await?;
    let client = server.client().await;
    let topic: Topic = TOPIC.parse()?;
    let key: PresenceKey = "ana".parse()?;
    let kv_key = EntryKey::new(topic.clone(), key.clone(), HolderId::generate()?).encode(ShardCount::DEFAULT)?;
    publish(&client, &kv_key, stored(&topic, &key)?).await?;
    wait_for_listing(&client, &topic, &keys(["ana"])).await?;

    presence_stream(&client)
        .await?
        .purge()
        .filter(presence_config().bucket().subject_for(&kv_key))
        .await?;
    wait_for_listing(&client, &topic, &BTreeSet::new()).await?;

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shard_owner_never_lists_control_records() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, service_config("edge-a")?).await?;
    let client = server.client().await;
    let topic: Topic = TOPIC.parse()?;
    let key: PresenceKey = "ana".parse()?;
    let holder = HolderId::generate()?;
    for control in [
        format!("ctl.writer.{}", key.token()),
        format!("ctl.direct.{holder}"),
        format!("ctl.receipt.direct.{holder}.AAAAAAAAAAAAAAAAAAAAAA"),
    ] {
        publish(&client, &KvKey::try_from(control)?, stored(&topic, &key)?).await?;
    }
    track(&client, &topic, "bob", "online").await?;
    wait_for_listing(&client, &topic, &keys(["bob"])).await?;

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shard_owner_refuses_a_shard_whose_replay_exceeds_the_budget() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = service_config("edge-a")?.with_rebuild_budget(RebuildBudget::try_from(Duration::from_millis(250))?);
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let topic: Topic = TOPIC.parse()?;
    let context = jetstream::new(client.clone());
    let mut acks = Vec::new();
    for index in 0..20_000 {
        let key: PresenceKey = format!("user-{index}").parse()?;
        let kv_key = EntryKey::new(topic.clone(), key.clone(), HolderId::generate()?).encode(ShardCount::DEFAULT)?;
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_TTL, LONG_TTL);
        acks.push(
            context
                .publish_with_headers(
                    presence_config().bucket().subject_for(&kv_key),
                    headers,
                    stored(&topic, &key)?.into(),
                )
                .await?,
        );
        if acks.len() == 1_000 {
            for ack in acks.drain(..) {
                ack.await?;
            }
        }
    }
    for ack in acks {
        ack.await?;
    }

    let handle = trogon_presence_service::start(client.clone(), config).await?;
    let flooded = ViewShard::of(&topic, ShardCount::DEFAULT);
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "no shard was owned")?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!handle.owned_shards().contains(&flooded));
    assert_eq!(listed_keys(&client, &topic).await.ok().flatten(), None);

    handle.shutdown().await;
    Ok(())
}
