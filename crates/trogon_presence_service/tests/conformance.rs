mod common;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use async_nats::{HeaderMap, Message, Subscriber};
use futures_util::stream::{select_all, SelectAll};
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::{
    ConnectionId, HolderId, PresenceConfig, PresenceKey, Presences, ProvisionOptions, RequestId, ShardCount, Topic,
    ViewShard, WriterMode,
};
use trogon_presence_service::reply::{
    HEADER_CODE, HEADER_GENERATION, HEADER_KIND, HEADER_OWNER_ID, HEADER_OWNER_REV, HEADER_PART, HEADER_PARTS,
    HEADER_PREV, HEADER_REQUEST_ID, HEADER_SEQ, HEADER_SNAPSHOT_ID,
};
use trogon_presence_service::snapshot::{Assembly, AssemblyProgress, SnapshotFrame, SnapshotManifest};
use trogon_presence_service::subjects::{diff_subject, epoch_subject, snapshot_subject, SnapshotReplySubject};
use trogon_presence_service::{
    NodeId, PayloadBudget, ServiceConfig, ServiceHandle, ShardLeaseTtl, SnapshotLimits, WriteOp, CALLER_INBOX_PREFIX,
};

use common::NatsServer;

type TestResult = Result<(), BoxError>;
type BoxError = Box<dyn Error + Send + Sync>;

const TOPIC: &str = "room:lobby";
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_millis(300);

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
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed))
}

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    Ok(ServiceConfig::new(presence_config()?, node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?))
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
    .map_err(|_| format!("owned {} shards, expected {expected}", handle.owned_shards().len()))?;
    common::wait_for_writers(&handle, expected, OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

fn header<'a>(headers: Option<&'a HeaderMap>, name: &str) -> Option<&'a str> {
    headers
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn number(message: &Message, name: &str) -> Result<u64, BoxError> {
    Ok(header(message.headers.as_ref(), name)
        .ok_or_else(|| format!("missing {name} on {}", message.subject))?
        .parse()?)
}

fn text(message: &Message, name: &str) -> Result<String, BoxError> {
    Ok(header(message.headers.as_ref(), name)
        .ok_or_else(|| format!("missing {name} on {}", message.subject))?
        .to_owned())
}

fn positioned(message: &Message, mut record: Value) -> Result<Value, BoxError> {
    record["generation"] = json!(text(message, HEADER_GENERATION)?);
    record["owner_rev"] = json!(number(message, HEADER_OWNER_REV)?);
    record["owner_id"] = json!(text(message, HEADER_OWNER_ID)?);
    Ok(record)
}

fn epoch_key(record: &Value) -> Value {
    json!([record["generation"], record["owner_rev"], record["owner_id"]])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    Record,
    Drop,
}

struct Recorder {
    client: async_nats::Client,
    shards: ShardCount,
    topic: Topic,
    caller: PresenceKey,
    connection: ConnectionId,
    limits: SnapshotLimits,
    frames: SelectAll<Subscriber>,
    records: Vec<Value>,
    entries: HashMap<(String, String), (Value, Value)>,
}

impl Recorder {
    async fn new(server: &NatsServer, scenario: &str, resyncs: u32, limits: SnapshotLimits) -> Result<Self, BoxError> {
        let client = server.client().await;
        let shards = ShardCount::DEFAULT;
        let topic: Topic = TOPIC.parse()?;
        let caller: PresenceKey = "recorder".parse()?;
        let connection = ConnectionId::generate()?;
        let diffs = client.subscribe(diff_subject(&topic)).await?;
        let epochs = client
            .subscribe(epoch_subject(shards, ViewShard::of(&topic, shards)))
            .await?;
        let parts = client
            .subscribe(SnapshotReplySubject::filter(&caller, &connection))
            .await?;
        client.flush().await?;
        Ok(Self {
            client,
            shards,
            topic,
            caller,
            connection,
            limits,
            frames: select_all([diffs, epochs, parts]),
            records: vec![json!({ "kind": "scenario", "name": scenario, "resyncs": resyncs })],
            entries: HashMap::new(),
        })
    }

    fn record(message: &Message) -> Result<Value, BoxError> {
        if message.subject.starts_with("presence.v1.epoch.") {
            return positioned(
                message,
                json!({
                    "kind": "epoch",
                    "payload": serde_json::from_slice::<Value>(&message.payload)?,
                }),
            );
        }
        let headers = message.headers.as_ref();
        let kind = header(headers, HEADER_KIND).ok_or("frame without Presence-Kind")?;
        if message.subject.starts_with("presence.v1.snapshot-reply.") {
            let mut record = json!({
                "kind": kind,
                "seq": number(message, HEADER_SEQ)?,
                "snapshot_id": text(message, HEADER_SNAPSHOT_ID)?,
                "request_id": text(message, HEADER_REQUEST_ID)?,
            });
            if header(headers, HEADER_PART).is_some() {
                record["part"] = json!(number(message, HEADER_PART)?);
                record["payload"] = json!(message.payload.to_vec());
            }
            if header(headers, HEADER_PARTS).is_some() {
                record["parts"] = json!(number(message, HEADER_PARTS)?);
            }
            return positioned(message, record);
        }
        let prev = match header(headers, HEADER_PREV) {
            Some(_) => Some(number(message, HEADER_PREV)?),
            None => None,
        };
        positioned(
            message,
            json!({
                "kind": kind,
                "seq": number(message, HEADER_SEQ)?,
                "prev": prev,
                "payload": serde_json::from_slice::<Value>(&message.payload)?,
            }),
        )
    }

    async fn next(&mut self, timeout: Duration) -> Result<Option<Message>, BoxError> {
        match tokio::time::timeout(timeout, self.frames.next()).await {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => Err("frame subscriptions ended".into()),
            Err(_) => Ok(None),
        }
    }

    async fn frame(&mut self, kind: &str, keep: Keep) -> Result<Value, BoxError> {
        loop {
            let message = self
                .next(FRAME_TIMEOUT)
                .await?
                .ok_or_else(|| format!("no {kind} frame within {FRAME_TIMEOUT:?}"))?;
            let record = Self::record(&message)?;
            let matches = record["kind"] == kind;
            if !(matches && keep == Keep::Drop) {
                self.records.push(record.clone());
            }
            if matches {
                return Ok(record);
            }
        }
    }

    async fn settle(&mut self, window: Duration) -> Result<Vec<Value>, BoxError> {
        let deadline = tokio::time::Instant::now() + window;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Ok(seen);
            }
            if let Some(message) = self.next(left).await? {
                let record = Self::record(&message)?;
                self.records.push(record.clone());
                seen.push(record);
            }
        }
    }

    async fn write(&mut self, op: WriteOp, key: &str, body: Value) -> Result<Value, BoxError> {
        let key: PresenceKey = key.parse()?;
        let reply = common::command(&self.client, op.subject(&key, &self.topic), &key, &body).await?;
        let code = header(reply.headers.as_ref(), HEADER_CODE);
        let reply: Value = serde_json::from_slice(&reply.payload)?;
        if code != Some("ok") {
            return Err(format!("{} {key} replied {code:?}: {reply}", op.as_str()).into());
        }
        let entry = (key.to_string(), body["holder"].as_str().unwrap_or_default().to_owned());
        if reply["lifetime"].is_string() {
            self.entries
                .insert(entry, (reply["lifetime"].clone(), reply["mutation_seq"].clone()));
        } else {
            self.entries.remove(&entry);
        }
        Ok(reply)
    }

    fn entry(&self, key: &str, holder: &HolderId) -> Result<(Value, Value), BoxError> {
        self.entries
            .get(&(key.to_owned(), holder.to_string()))
            .cloned()
            .ok_or_else(|| format!("no tracked entry for {key}").into())
    }

    async fn track(&mut self, key: &str, holder: &HolderId, meta: Value) -> Result<Value, BoxError> {
        self.write(WriteOp::Track, key, json!({ "holder": holder, "meta": meta }))
            .await
    }

    async fn update(&mut self, key: &str, holder: &HolderId, meta: Value) -> Result<Value, BoxError> {
        let (lifetime, mutation_seq) = self.entry(key, holder)?;
        self.write(
            WriteOp::Update,
            key,
            json!({ "holder": holder, "meta": meta, "lifetime": lifetime, "mutation_seq": mutation_seq }),
        )
        .await
    }

    async fn untrack(&mut self, key: &str, holder: &HolderId) -> Result<Value, BoxError> {
        let (lifetime, mutation_seq) = self.entry(key, holder)?;
        self.write(
            WriteOp::Untrack,
            key,
            json!({ "holder": holder, "lifetime": lifetime, "mutation_seq": mutation_seq }),
        )
        .await
    }

    async fn snapshot(&mut self, keep: Keep) -> Result<(Value, Presences), BoxError> {
        let request = RequestId::generate()?;
        let inbox = format!(
            "{CALLER_INBOX_PREFIX}.{}.{}.{request}",
            self.caller.token(),
            self.connection
        );
        let mut replies = self.client.subscribe(inbox.clone()).await?;
        self.client.flush().await?;
        self.client
            .publish_with_reply(
                snapshot_subject(self.shards, &self.caller, &self.connection, &self.topic),
                inbox,
                serde_json::to_vec(&json!({ "request_id": request }))?.into(),
            )
            .await?;
        let reply = tokio::time::timeout(FRAME_TIMEOUT, replies.next())
            .await?
            .ok_or("snapshot inbox ended")?;
        replies.unsubscribe().await?;
        let code = header(reply.headers.as_ref(), HEADER_CODE);
        if code != Some("ok") {
            let body = String::from_utf8_lossy(&reply.payload);
            return Err(format!("snapshot replied {code:?}: {body}").into());
        }
        let manifest: SnapshotManifest = serde_json::from_slice(&reply.payload)?;
        let record = positioned(
            &reply,
            json!({
                "kind": header(reply.headers.as_ref(), HEADER_KIND).ok_or("manifest without Presence-Kind")?,
                "seq": number(&reply, HEADER_SEQ)?,
                "payload": serde_json::from_slice::<Value>(&reply.payload)?,
            }),
        )?;
        if keep == Keep::Record {
            self.records.push(record.clone());
        }
        let snapshot_id = manifest.identity().snapshot().to_string();
        let mut assembly = Assembly::begin(manifest, self.limits)?;
        loop {
            let message = self.next(FRAME_TIMEOUT).await?.ok_or("snapshot did not complete")?;
            let framed = Self::record(&message)?;
            let ours = framed["snapshot_id"] == snapshot_id.as_str();
            if keep == Keep::Record || !ours {
                self.records.push(framed);
            }
            if !ours {
                continue;
            }
            if let AssemblyProgress::Complete(state) = assembly.accept(SnapshotFrame::try_from(&message)?)? {
                return Ok((record, state));
            }
        }
    }

    async fn resync_reply(&mut self) -> Result<(Value, Presences), BoxError> {
        self.snapshot(Keep::Record).await
    }

    async fn finish(mut self, scenario: &str) -> TestResult {
        self.settle(SETTLE).await?;
        let (manifest, state) = self.snapshot(Keep::Drop).await?;
        let mut expected = manifest;
        expected["kind"] = json!("expected");
        expected["payload"] = serde_json::to_value(&state)?;
        self.records.push(expected);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance/golden");
        std::fs::create_dir_all(&dir)?;
        let mut out = String::new();
        for record in &self.records {
            out.push_str(&serde_json::to_string(record)?);
            out.push('\n');
        }
        std::fs::write(dir.join(format!("{scenario}.jsonl")), out)?;
        Ok(())
    }
}

fn holder() -> Result<HolderId, BoxError> {
    Ok(HolderId::generate()?)
}

fn joined(frame: &Value) -> Vec<String> {
    frame["payload"]["joins"]
        .as_object()
        .map(|joins| joins.keys().cloned().collect())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites conformance/golden, run with `mise run presence:conformance:record`"]
async fn joins_leaves_and_updates() -> TestResult {
    let scenario = "joins_leaves_and_updates";
    let server = server_or_skip!();
    let service = start_owning_all(&server, service_config("edge-a")?).await?;
    let mut rec = Recorder::new(&server, scenario, 0, SnapshotLimits::default()).await?;
    rec.resync_reply().await?;

    let (ana_web, ana_phone, bob) = (holder()?, holder()?, holder()?);
    rec.track("ana", &ana_web, json!({ "status": "online", "device": "web" }))
        .await?;
    rec.frame("diff", Keep::Record).await?;
    rec.track("bob", &bob, json!({ "status": "away" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.track("ana", &ana_phone, json!({ "status": "online", "device": "phone" }))
        .await?;
    rec.frame("diff", Keep::Record).await?;
    rec.update("ana", &ana_web, json!({ "status": "busy", "device": "web" }))
        .await?;
    let update = rec.frame("diff", Keep::Record).await?;
    assert_eq!(joined(&update), ["ana"], "{update}");
    rec.update("ana", &ana_web, json!({ "status": "online", "device": "web" }))
        .await?;
    rec.frame("diff", Keep::Record).await?;
    rec.untrack("bob", &bob).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.untrack("ana", &ana_phone).await?;
    rec.frame("diff", Keep::Record).await?;
    let prototype = holder()?;
    rec.track("constructor", &prototype, json!({ "status": "online" }))
        .await?;
    rec.frame("diff", Keep::Record).await?;

    rec.finish(scenario).await?;
    service.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites conformance/golden, run with `mise run presence:conformance:record`"]
async fn dropped_diff_forces_list_resync() -> TestResult {
    let scenario = "dropped_diff_forces_list_resync";
    let server = server_or_skip!();
    let service = start_owning_all(&server, service_config("edge-a")?).await?;
    let mut rec = Recorder::new(&server, scenario, 1, SnapshotLimits::default()).await?;
    rec.resync_reply().await?;

    let (ana, bob, carol) = (holder()?, holder()?, holder()?);
    rec.track("ana", &ana, json!({ "status": "online" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.track("bob", &bob, json!({ "status": "online" })).await?;
    rec.frame("diff", Keep::Drop).await?;
    rec.update("ana", &ana, json!({ "status": "away" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.update("bob", &bob, json!({ "status": "busy" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.resync_reply().await?;
    rec.track("carol", &carol, json!({ "status": "online" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.untrack("ana", &ana).await?;
    rec.frame("diff", Keep::Record).await?;

    rec.finish(scenario).await?;
    service.shutdown().await;
    Ok(())
}

async fn takeover(
    server: &NatsServer,
    rec: &mut Recorder,
    previous: ServiceHandle,
    next: ServiceConfig,
) -> Result<(ServiceHandle, Value, Presences), BoxError> {
    let before = rec
        .records
        .iter()
        .rev()
        .find(|record| record["kind"] == "diff")
        .map(epoch_key)
        .ok_or("no diff before the takeover")?;
    previous.shutdown().await;
    let service = start_owning_all(server, next).await?;
    let announced = loop {
        let keepalive = rec.frame("keepalive", Keep::Record).await?;
        if epoch_key(&keepalive) != before {
            break keepalive;
        }
    };
    assert_eq!(announced["seq"], 1, "{announced}");
    assert_eq!(announced["prev"], 1, "{announced}");
    rec.settle(SETTLE).await?;
    assert!(
        rec.records.iter().any(|record| record["kind"] == "epoch"),
        "no epoch announcement was recorded"
    );
    let (manifest, state) = rec.resync_reply().await?;
    assert_eq!(epoch_key(&manifest), epoch_key(&announced), "{manifest}");
    Ok((service, manifest, state))
}

fn keys_of(state: &Presences) -> Vec<String> {
    state.iter().map(|(key, _)| key.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites conformance/golden, run with `mise run presence:conformance:record`"]
async fn epoch_change_on_takeover() -> TestResult {
    let scenario = "epoch_change_on_takeover";
    let server = server_or_skip!();
    let first = start_owning_all(&server, service_config("edge-a")?).await?;
    let mut rec = Recorder::new(&server, scenario, 1, SnapshotLimits::default()).await?;
    rec.resync_reply().await?;

    let (ana, bob, carol) = (holder()?, holder()?, holder()?);
    rec.track("ana", &ana, json!({ "status": "online" })).await?;
    let before = rec.frame("diff", Keep::Record).await?;
    rec.track("bob", &bob, json!({ "status": "away" })).await?;
    rec.frame("diff", Keep::Record).await?;

    let (second, manifest, state) = takeover(&server, &mut rec, first, service_config("edge-b")?).await?;
    assert_ne!(epoch_key(&manifest), epoch_key(&before), "{manifest}");
    assert_eq!(keys_of(&state), ["ana", "bob"]);

    rec.track("carol", &carol, json!({ "status": "online" })).await?;
    let after = rec.frame("diff", Keep::Record).await?;
    assert_eq!(after["prev"], manifest["seq"], "{after}");
    assert_eq!(epoch_key(&after), epoch_key(&manifest), "{after}");
    rec.update("carol", &carol, json!({ "status": "busy" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.update("ana", &ana, json!({ "status": "away" })).await?;
    rec.frame("diff", Keep::Record).await?;

    rec.finish(scenario).await?;
    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites conformance/golden, run with `mise run presence:conformance:record`"]
async fn chunked_snapshot() -> TestResult {
    let scenario = "chunked_snapshot";
    let server = server_or_skip!();
    let limits = SnapshotLimits::default().with_payload(PayloadBudget::try_from(1024)?);
    let first = start_owning_all(&server, service_config("edge-a")?.with_snapshot_limits(limits)).await?;
    let mut rec = Recorder::new(&server, scenario, 1, limits).await?;
    rec.resync_reply().await?;

    let bio = "x".repeat(200);
    for index in 0..4 {
        let member = holder()?;
        rec.track(
            &format!("member-{index}"),
            &member,
            json!({ "status": "online", "bio": bio }),
        )
        .await?;
        rec.frame("diff", Keep::Record).await?;
    }

    let next = service_config("edge-b")?.with_snapshot_limits(limits);
    let (second, manifest, state) = takeover(&server, &mut rec, first, next).await?;
    assert_eq!(state.len(), 4);
    let parts = manifest["payload"]["parts"].as_u64().ok_or("manifest without parts")?;
    assert!(parts > 1, "snapshot was not chunked: {manifest}");
    let recorded = rec
        .records
        .iter()
        .filter(|record| {
            record["kind"] == "snapshot-part" && record["snapshot_id"] == manifest["payload"]["snapshot_id"]
        })
        .count();
    assert_eq!(u64::try_from(recorded)?, parts);

    let late = holder()?;
    rec.track("member-late", &late, json!({ "status": "online" })).await?;
    rec.frame("diff", Keep::Record).await?;

    rec.finish(scenario).await?;
    second.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites conformance/golden, run with `mise run presence:conformance:record`"]
async fn keepalive_only_stretch() -> TestResult {
    let scenario = "keepalive_only_stretch";
    let server = server_or_skip!();
    let config = service_config("edge-a")?.with_keepalive(trogon_presence_service::KeepaliveInterval::try_from(
        Duration::from_secs(1),
    )?);
    let service = start_owning_all(&server, config).await?;
    let mut rec = Recorder::new(&server, scenario, 0, SnapshotLimits::default()).await?;
    rec.resync_reply().await?;

    let ana = holder()?;
    rec.track("ana", &ana, json!({ "status": "online" })).await?;
    let diff = rec.frame("diff", Keep::Record).await?;
    let quiet = rec.settle(Duration::from_millis(3500)).await?;
    let keepalives: Vec<&Value> = quiet.iter().filter(|record| record["kind"] == "keepalive").collect();
    assert!(keepalives.len() >= 2, "expected keepalives, saw {quiet:?}");
    assert!(quiet.iter().all(|record| record["kind"] == "keepalive"), "{quiet:?}");
    for keepalive in keepalives {
        assert_eq!(keepalive["seq"], diff["seq"]);
        assert_eq!(keepalive["prev"], diff["seq"]);
    }
    rec.update("ana", &ana, json!({ "status": "away" })).await?;
    rec.frame("diff", Keep::Record).await?;
    rec.settle(Duration::from_millis(1500)).await?;

    rec.finish(scenario).await?;
    service.shutdown().await;
    Ok(())
}
