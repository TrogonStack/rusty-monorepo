mod common;

use std::error::Error;
use std::time::Duration;

use async_nats::{Message, Subscriber};
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trogon_presence::{
    ConnectionId, HolderId, PresenceConfig, PresenceKey, ProvisionOptions, RequestId, ShardCount, Topic, WriterMode,
};
use trogon_presence_service::reply::{HEADER_CODE, HEADER_KIND, HEADER_PART, HEADER_PARTS, HEADER_REQUEST_ID};
use trogon_presence_service::snapshot::{
    header_wire_len, AssembliesPerConnection, AssembliesPerProcess, AssemblyRefusal, SnapshotBytes, SnapshotManifest,
    SnapshotMaxBytes,
};
use trogon_presence_service::subjects::{snapshot_subject, SnapshotReplySubject};
use trogon_presence_service::{
    AssemblyGate, NodeId, PayloadBudget, ServiceConfig, ServiceHandle, ShardLeaseTtl, SnapshotLimits, WriteOp,
    CALLER_INBOX_PREFIX,
};

use common::NatsServer;

type BoxError = Box<dyn Error + Send + Sync>;
type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(500);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn service_config(limits: SnapshotLimits) -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed);
    Ok(ServiceConfig::new(presence, "edge-a".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_snapshot_limits(limits))
}

async fn start(server: &NatsServer, limits: SnapshotLimits) -> Result<ServiceHandle, BoxError> {
    let config = service_config(limits)?;
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
    .map_err(|_| "service never owned every shard")?;
    common::wait_for_writers(&handle, expected, OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

async fn track(client: &async_nats::Client, topic: &Topic, key: &str, bio: &str) -> TestResult {
    let key: PresenceKey = key.parse()?;
    let reply = common::command(
        client,
        WriteOp::Track.subject(&key, topic),
        &key,
        &json!({ "holder": HolderId::generate()?, "meta": { "status": "online", "bio": bio } }),
    )
    .await?;
    match common::code_of(&reply) {
        Some("ok") => Ok(()),
        code => Err(format!("track {key} replied {code:?}").into()),
    }
}

fn header<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    message
        .headers
        .as_ref()
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

struct Caller {
    client: async_nats::Client,
    key: PresenceKey,
    connection: ConnectionId,
    topic: Topic,
    frames: Subscriber,
}

impl Caller {
    async fn new(server: &NatsServer) -> Result<Self, BoxError> {
        let client = server.client().await;
        let key: PresenceKey = "observer".parse()?;
        let connection = ConnectionId::generate()?;
        let frames = client
            .subscribe(SnapshotReplySubject::filter(&key, &connection))
            .await?;
        client.flush().await?;
        Ok(Self {
            client,
            key,
            connection,
            topic: TOPIC.parse()?,
            frames,
        })
    }

    fn inbox(&self, key: &PresenceKey, connection: &ConnectionId) -> String {
        format!("{CALLER_INBOX_PREFIX}.{}.{connection}.view", key.token())
    }

    async fn request_via(&self, inbox: String, within: Duration) -> Result<Option<(RequestId, Message)>, BoxError> {
        let request = RequestId::generate()?;
        let mut replies = self.client.subscribe(inbox.clone()).await?;
        self.client.flush().await?;
        self.client
            .publish_with_reply(
                snapshot_subject(ShardCount::DEFAULT, &self.key, &self.connection, &self.topic),
                inbox,
                serde_json::to_vec(&json!({ "request_id": request }))?.into(),
            )
            .await?;
        let reply = tokio::time::timeout(within, replies.next()).await;
        replies.unsubscribe().await?;
        match reply {
            Ok(Some(message)) => Ok(Some((request, message))),
            Ok(None) => Err("reply subscription ended".into()),
            Err(_) => Ok(None),
        }
    }

    async fn request(&self) -> Result<(RequestId, Message), BoxError> {
        self.request_via(self.inbox(&self.key, &self.connection), WAIT)
            .await?
            .ok_or_else(|| "no snapshot reply".into())
    }

    async fn collect(&mut self, request: RequestId) -> Result<(Vec<Message>, Message), BoxError> {
        let mut parts = Vec::new();
        loop {
            let frame = tokio::time::timeout(WAIT, self.frames.next())
                .await?
                .ok_or("snapshot frames ended")?;
            assert_eq!(header(&frame, HEADER_REQUEST_ID), Some(request.to_string().as_str()));
            match header(&frame, HEADER_KIND) {
                Some("snapshot-part") => parts.push(frame),
                Some("snapshot-end") => return Ok((parts, frame)),
                other => return Err(format!("unexpected snapshot frame kind {other:?}").into()),
            }
        }
    }
}

fn manifest_of(reply: &Message) -> Result<Value, BoxError> {
    assert_eq!(header(reply, HEADER_CODE), Some("ok"));
    assert_eq!(header(reply, HEADER_KIND), Some("manifest"));
    serde_json::from_slice::<SnapshotManifest>(&reply.payload)?;
    Ok(serde_json::from_slice(&reply.payload)?)
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_digest_and_part_count_match_the_chunks() -> TestResult {
    let server = server_or_skip!();
    let limits = SnapshotLimits::default().with_payload(PayloadBudget::try_from(1024)?);
    let handle = start(&server, limits).await?;
    let mut caller = Caller::new(&server).await?;
    let bio = "x".repeat(200);
    for index in 0..6 {
        track(&caller.client, &caller.topic, &format!("member-{index}"), &bio).await?;
    }

    let (request, reply) = caller.request().await?;
    let manifest = manifest_of(&reply)?;
    assert_eq!(manifest["request_id"], request.to_string());
    let (parts, end) = caller.collect(request).await?;
    let count = manifest["parts"].as_u64().ok_or("manifest without parts")?;
    assert!(count > 1, "{manifest}");
    assert_eq!(u64::try_from(parts.len())?, count);
    assert_eq!(header(&end, HEADER_PARTS), Some(count.to_string().as_str()));
    let mut hasher = Sha256::new();
    let mut body = Vec::new();
    for (expected, part) in (1u64..).zip(&parts) {
        assert_eq!(header(part, HEADER_PART), Some(expected.to_string().as_str()));
        let headers = part.headers.as_ref().ok_or("snapshot part without headers")?;
        assert!(header_wire_len(headers) + part.payload.len() <= 1024);
        hasher.update(&part.payload);
        body.extend_from_slice(&part.payload);
    }
    let digest: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(manifest["sha256"], digest);
    assert_eq!(manifest["total_bytes"], json!(body.len()));
    let state: Value = serde_json::from_slice(&body)?;
    assert_eq!(state.as_object().map(serde_json::Map::len), Some(6));

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_topic_is_one_empty_object_part_and_an_end() -> TestResult {
    let server = server_or_skip!();
    let handle = start(&server, SnapshotLimits::default()).await?;
    let mut caller = Caller::new(&server).await?;

    let (request, reply) = caller.request().await?;
    let manifest = manifest_of(&reply)?;
    assert_eq!(manifest["parts"], 1);
    assert_eq!(manifest["total_bytes"], 2);
    let (parts, end) = caller.collect(request).await?;
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].payload.as_ref(), b"{}");
    assert_eq!(header(&end, HEADER_PARTS), Some("1"));
    assert!(end.payload.is_empty());

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_topic_above_the_cap_is_snapshot_too_large() -> TestResult {
    let server = server_or_skip!();
    let limits = SnapshotLimits::default().with_max_bytes(SnapshotMaxBytes::try_from(128)?);
    let handle = start(&server, limits).await?;
    let caller = Caller::new(&server).await?;
    let bio = "x".repeat(200);
    track(&caller.client, &caller.topic, "ana", &bio).await?;

    let refused = tokio::time::timeout(WAIT, async {
        loop {
            let (_, reply) = caller.request().await?;
            let body: Value = serde_json::from_slice(&reply.payload)?;
            if body["error"] == "snapshot_too_large" {
                return Ok::<_, BoxError>(reply);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "snapshot never exceeded the cap")??;
    assert_ne!(header(&refused, HEADER_CODE), Some("ok"));

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_reply_prefix_is_rejected() -> TestResult {
    let server = server_or_skip!();
    let handle = start(&server, SnapshotLimits::default()).await?;
    let caller = Caller::new(&server).await?;
    let before = handle.stats().rejected_inbox();

    let stranger: PresenceKey = "stranger".parse()?;
    let foreign_key = caller.inbox(&stranger, &caller.connection);
    assert!(caller.request_via(foreign_key, QUIET).await?.is_none());
    let foreign_connection = caller.inbox(&caller.key, &ConnectionId::generate()?);
    assert!(caller.request_via(foreign_connection, QUIET).await?.is_none());
    let bare = format!("{CALLER_INBOX_PREFIX}.{}.{}", caller.key.token(), caller.connection);
    assert!(caller.request_via(bare, QUIET).await?.is_none());
    assert_eq!(handle.stats().rejected_inbox(), before + 3);

    assert!(caller.request().await.is_ok());
    handle.shutdown().await;
    Ok(())
}

#[test]
fn the_third_assembly_on_a_connection_is_refused() -> TestResult {
    let gate = AssemblyGate::new(SnapshotLimits::default());
    let connection = ConnectionId::generate()?;
    let bytes = SnapshotBytes::from(1024usize);
    let first = gate.admit(&connection, bytes)?;
    let _second = gate.admit(&connection, bytes)?;
    assert_eq!(gate.admit(&connection, bytes).err(), Some(AssemblyRefusal::Connection));
    assert!(gate.admit(&ConnectionId::generate()?, bytes).is_ok());
    drop(first);
    assert!(gate.admit(&connection, bytes).is_ok());
    assert_eq!(AssembliesPerConnection::default().get(), 2);
    Ok(())
}

#[test]
fn the_ninth_assembly_in_a_process_is_refused() -> TestResult {
    let gate = AssemblyGate::new(SnapshotLimits::default());
    let bytes = SnapshotBytes::from(1024usize);
    let mut permits = Vec::new();
    for _ in 0..8 {
        permits.push(gate.admit(&ConnectionId::generate()?, bytes)?);
    }
    assert_eq!(
        gate.admit(&ConnectionId::generate()?, bytes).err(),
        Some(AssemblyRefusal::Process)
    );
    permits.pop();
    assert!(gate.admit(&ConnectionId::generate()?, bytes).is_ok());
    assert_eq!(AssembliesPerProcess::default().get(), 8);
    Ok(())
}
