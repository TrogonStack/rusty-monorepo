mod common;

use std::collections::BTreeMap;
use std::error::Error;
use std::time::Duration;

use futures_util::future::join_all;
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::{HolderId, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic, WriterMode};
use trogon_presence_service::{
    KeepaliveInterval, NodeId, ReadOp, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
};

use common::NatsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const WRITERS: usize = 200;
const ROOMS: usize = 10;
const LOBBY: &str = "room:lobby";
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(30);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn service_config() -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed);
    Ok(ServiceConfig::new(presence, "burst".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

async fn start_owning_all(server: &NatsServer) -> Result<ServiceHandle, BoxError> {
    let config = service_config()?;
    let client = server.client().await;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    let shards = usize::from(ShardCount::DEFAULT.get());
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != shards {
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .map_err(|_| "the service did not take every view shard")?;
    common::wait_for_writers(&handle, shards, OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

struct Reply {
    code: String,
    body: Value,
}

async fn send(client: &async_nats::Client, subject: String, key: &PresenceKey, body: Value) -> Result<Reply, BoxError> {
    let inbox = common::caller_inbox(key)?;
    let mut replies = client.subscribe(inbox.clone()).await?;
    client
        .publish_with_reply(subject, inbox, serde_json::to_vec(&body)?.into())
        .await?;
    client.flush().await?;
    let message = tokio::time::timeout(REPLY_TIMEOUT, replies.next())
        .await
        .map_err(|_| format!("no reply for {key}"))?
        .ok_or("reply subscription ended")?;
    Ok(Reply {
        code: common::code_of(&message).unwrap_or("none").to_owned(),
        body: common::body_of(&message)?,
    })
}

struct Entry {
    key: PresenceKey,
    topic: Topic,
    holder: HolderId,
    tracked: Value,
}

fn topics_of(writer: usize) -> Result<[Topic; 2], BoxError> {
    Ok([LOBBY.parse()?, format!("room:{}", writer % ROOMS).parse()?])
}

async fn track(client: &async_nats::Client, writer: usize) -> Result<Vec<Entry>, BoxError> {
    let key: PresenceKey = format!("writer{writer}").parse()?;
    let holder = HolderId::generate()?;
    let mut entries = Vec::new();
    for topic in topics_of(writer)? {
        let reply = send(
            client,
            WriteOp::Track.subject(&key, &topic),
            &key,
            json!({ "holder": holder, "meta": { "status": "online" } }),
        )
        .await?;
        if reply.code != "ok" {
            return Err(format!("track {key} on {topic} replied {} {}", reply.code, reply.body).into());
        }
        entries.push(Entry {
            key: key.clone(),
            topic,
            holder,
            tracked: reply.body,
        });
    }
    Ok(entries)
}

async fn untrack(client: &async_nats::Client, entries: &[Entry]) -> Result<Vec<Reply>, BoxError> {
    let mut replies = Vec::new();
    for entry in entries {
        replies.push(
            send(
                client,
                WriteOp::Untrack.subject(&entry.key, &entry.topic),
                &entry.key,
                json!({
                    "holder": entry.holder,
                    "lifetime": entry.tracked["lifetime"],
                    "mutation_seq": entry.tracked["mutation_seq"],
                }),
            )
            .await?,
        );
    }
    Ok(replies)
}

async fn listed(client: &async_nats::Client, topic: &Topic) -> Result<usize, BoxError> {
    let caller: PresenceKey = "observer".parse()?;
    let reply = client
        .request(
            ReadOp::List.subject(ShardCount::DEFAULT, &caller, topic),
            Vec::new().into(),
        )
        .await?;
    if common::code_of(&reply) != Some("ok") {
        return Err(format!("list {topic} replied {:?}", common::code_of(&reply)).into());
    }
    let state: Value = serde_json::from_slice(&reply.payload)?;
    Ok(state.as_object().map_or(0, |entries| entries.len()))
}

async fn lingering(client: &async_nats::Client) -> Result<BTreeMap<String, usize>, BoxError> {
    let mut topics: Vec<Topic> = vec![LOBBY.parse()?];
    for room in 0..ROOMS {
        topics.push(format!("room:{room}").parse()?);
    }
    let mut held = BTreeMap::new();
    for topic in topics {
        let count = listed(client, &topic).await?;
        if count > 0 {
            held.insert(topic.to_string(), count);
        }
    }
    Ok(held)
}

async fn settle(client: &async_nats::Client, expected: usize) -> Result<(), BoxError> {
    let settled = tokio::time::timeout(DRAIN_TIMEOUT, async {
        loop {
            if lingering(client).await?.values().sum::<usize>() == expected {
                return Ok::<_, BoxError>(());
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    match settled {
        Ok(result) => result,
        Err(_) => Err(format!(
            "expected {expected} listed entries, found {:?}",
            lingering(client).await?
        )
        .into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_untrack_burst_never_surfaces_unavailable() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server).await?;
    let client = server.client().await;

    let tracked = join_all((0..WRITERS).map(|writer| track(&client, writer))).await;
    let writers = tracked.into_iter().collect::<Result<Vec<_>, _>>()?;
    settle(&client, WRITERS * 2).await?;

    let replies = join_all(writers.iter().map(|entries| untrack(&client, entries))).await;
    let mut codes: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = Vec::new();
    for reply in replies
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
    {
        *codes.entry(reply.code.clone()).or_default() += 1;
        if reply.code != "ok" {
            failures.push(reply.body);
        }
    }
    assert_eq!(codes.get("unavailable"), None, "{codes:?} {failures:?}");
    assert_eq!(codes, BTreeMap::from([("ok".to_owned(), WRITERS * 2)]), "{failures:?}");

    settle(&client, 0).await?;

    handle.shutdown().await;
    Ok(())
}
