mod common;

use std::collections::BTreeMap;
use std::error::Error;
use std::time::Duration;

use futures_util::future::join_all;
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::{HolderId, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic, WriterMode};
use trogon_presence_service::{KeepaliveInterval, NodeId, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp};

use common::{Access, NatsServer};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const WRITERS: usize = 200;
const ROOMS: usize = 10;
const LOBBY: &str = "room:lobby";
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(30);
const HANDOFF_WITHIN: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(20);

macro_rules! restricted_server_or_skip {
    () => {
        match NatsServer::start_with(Access::RuntimeDeniedPlainInbox).await {
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
    Ok(ServiceConfig::new(presence, "handoff".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

async fn start_owning_all(server: &NatsServer) -> Result<ServiceHandle, BoxError> {
    let config = service_config()?;
    let client = server.runtime_client().await;
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

async fn track(client: &async_nats::Client, writer: usize) -> Result<(Vec<Entry>, Vec<Reply>), BoxError> {
    let key: PresenceKey = format!("writer{writer}").parse()?;
    let holder = HolderId::generate()?;
    let mut entries = Vec::new();
    let mut refused = Vec::new();
    for topic in topics_of(writer)? {
        let reply = send(
            client,
            WriteOp::Track.subject(&key, &topic),
            &key,
            json!({ "holder": holder, "meta": { "status": "online" } }),
        )
        .await?;
        if reply.code == "ok" {
            entries.push(Entry {
                key: key.clone(),
                topic,
                holder,
                tracked: reply.body,
            });
        } else {
            refused.push(reply);
        }
    }
    Ok((entries, refused))
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

fn tally<'a>(replies: impl IntoIterator<Item = &'a Reply>) -> BTreeMap<String, usize> {
    let mut codes = BTreeMap::new();
    for reply in replies {
        let detail = reply.body["detail"].as_str().unwrap_or_default();
        *codes
            .entry(format!("{} {detail}", reply.code).trim().to_owned())
            .or_insert(0) += 1;
    }
    codes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_writer_confirms_a_hand_off_to_a_least_privilege_runtime() -> TestResult {
    let server = restricted_server_or_skip!();
    let handle = start_owning_all(&server).await?;
    let client = server.client().await;
    let key: PresenceKey = "solo".parse()?;
    let topic: Topic = LOBBY.parse()?;
    let reply = send(
        &client,
        WriteOp::Track.subject(&key, &topic),
        &key,
        json!({ "holder": HolderId::generate()?, "meta": {} }),
    )
    .await?;
    assert_eq!(reply.code, "ok", "{}", reply.body);
    let confirmed = tokio::time::timeout(HANDOFF_WITHIN, async {
        while handle.stats().forwarded() == 0 {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    let stats = handle.stats();
    handle.shutdown().await;
    assert!(
        confirmed.is_ok(),
        "the ingress never saw the writer confirm the hand-off: forwarded {} of {} admitted",
        stats.forwarded(),
        stats.admitted()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lobby_then_room_burst_is_never_refused_at_ingress() -> TestResult {
    let server = restricted_server_or_skip!();
    let handle = start_owning_all(&server).await?;
    let client = server.client().await;

    let tracked = join_all((0..WRITERS).map(|writer| track(&client, writer))).await;
    let mut entries = Vec::new();
    let mut refused = Vec::new();
    for outcome in tracked {
        let (mine, theirs) = outcome?;
        entries.push(mine);
        refused.extend(theirs);
    }
    let untracked = join_all(entries.iter().map(|mine| untrack(&client, mine))).await;
    let mut untrack_replies = Vec::new();
    for outcome in untracked {
        untrack_replies.extend(outcome?);
    }
    let stats = handle.stats();
    handle.shutdown().await;

    assert!(refused.is_empty(), "tracks refused: {:?}", tally(&refused));
    let codes = tally(&untrack_replies);
    assert_eq!(
        codes.get("ok").copied(),
        Some(WRITERS * 2),
        "untrack replies: {codes:?}"
    );
    assert_eq!(stats.overloaded(), 0, "ingress refused commands: {codes:?}");
    Ok(())
}
