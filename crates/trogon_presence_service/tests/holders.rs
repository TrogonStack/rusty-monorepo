mod common;

use std::error::Error;
use std::time::Duration;

use serde_json::{json, Value};
use trogon_presence::{
    HeartbeatInterval, HolderId, LeaseTtl, MarkerTtl, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic,
    WriterMode,
};
use trogon_presence_service::reply::HEADER_CODE;
use trogon_presence_service::subjects::HolderOp;
use trogon_presence_service::{
    KeepaliveInterval, NodeId, ReadOp, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
};

use common::NatsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const TOPIC: &str = "room:lobby";
const LEASE_TTL: Duration = Duration::from_secs(3);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(10);

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

async fn start_owning_all(server: &NatsServer, node: &str) -> Result<ServiceHandle, BoxError> {
    let client = server.client().await;
    let config = service_config(node)?;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    let expected = usize::from(ShardCount::DEFAULT.get());
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != expected {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "service did not take every shard")?;
    common::wait_for_writers(&handle, expected, OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

#[derive(Clone)]
struct Entry {
    topic: String,
    lifetime: Value,
    mutation_seq: Value,
    phx_ref: Value,
}

impl Entry {
    fn beat(&self, holder: HolderId) -> Value {
        json!({
            "holder": holder,
            "topic": self.topic,
            "lifetime": self.lifetime,
            "mutation_seq": self.mutation_seq,
        })
    }
}

struct Probe {
    client: async_nats::Client,
    shards: ShardCount,
}

impl Probe {
    async fn new(server: &NatsServer) -> Result<Self, BoxError> {
        Ok(Self {
            client: server.client().await,
            shards: presence_config()?.shards(),
        })
    }

    async fn request(&self, subject: String, key: &str, body: Value) -> Result<(String, Value), BoxError> {
        let key: PresenceKey = key.parse()?;
        let reply = common::command(&self.client, subject, &key, &body).await?;
        let code = common::code_of(&reply).ok_or("reply has no code")?.to_owned();
        Ok((code, common::body_of(&reply)?))
    }

    async fn write(&self, op: WriteOp, key: &str, topic: &str, body: Value) -> Result<(String, Value), BoxError> {
        self.request(op.subject(&key.parse()?, &topic.parse()?), key, body)
            .await
    }

    async fn track(&self, key: &str, topic: &str, holder: HolderId) -> Result<Entry, BoxError> {
        let (code, body) = self
            .write(
                WriteOp::Track,
                key,
                topic,
                json!({ "holder": holder, "meta": { "status": "online" } }),
            )
            .await?;
        assert_eq!(code, "ok", "{body}");
        Ok(Entry {
            topic: topic.to_owned(),
            lifetime: body["lifetime"].clone(),
            mutation_seq: body["mutation_seq"].clone(),
            phx_ref: body["phx_ref"].clone(),
        })
    }

    async fn holder_op(&self, op: HolderOp, key: &str, body: Value) -> Result<(String, Value), BoxError> {
        self.request(op.subject(&key.parse::<PresenceKey>()?), key, body).await
    }

    async fn listed(&self, topic: &str) -> Result<Option<Value>, BoxError> {
        let caller: PresenceKey = "observer".parse()?;
        let topic: Topic = topic.parse()?;
        let reply = self
            .client
            .request(ReadOp::List.subject(self.shards, &caller, &topic), Vec::new().into())
            .await?;
        let code = reply
            .headers
            .as_ref()
            .and_then(|headers| headers.get(HEADER_CODE))
            .map(|value| value.as_str().to_owned());
        if code.as_deref() != Some("ok") {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&reply.payload)?))
    }

    async fn statuses(&self, key: &str, topic: &str) -> Result<Option<Vec<Value>>, BoxError> {
        Ok(self.listed(topic).await?.map(|state| {
            state[key]["metas"]
                .as_array()
                .map(|metas| metas.iter().map(|meta| meta["status"].clone()).collect())
                .unwrap_or_default()
        }))
    }

    async fn wait_for(&self, key: &str, topic: &str, expected: Vec<Value>) -> Result<(), BoxError> {
        let deadline = tokio::time::Instant::now() + CONVERGE_TIMEOUT;
        loop {
            let seen = self.statuses(key, topic).await?;
            if seen.as_ref() == Some(&expected) {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!("{key} on {topic} listed {seen:?}, expected {expected:?}").into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn update_and_untrack_succeed_on_any_instance() -> TestResult {
    let server = server_or_skip!();
    let first = start_owning_all(&server, "edge-a").await?;
    let probe = Probe::new(&server).await?;
    let holder = HolderId::generate()?;
    let keys: Vec<String> = (0..8).map(|index| format!("user{index}")).collect();
    let mut entries = Vec::new();
    for key in &keys {
        entries.push(probe.track(key, TOPIC, holder).await?);
    }

    let second = trogon_presence_service::start(server.client().await, service_config("edge-b")?).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        second.writer_shards().is_empty(),
        "second instance took held writer leases"
    );

    let mut updated = Vec::new();
    for (key, entry) in keys.iter().zip(&entries) {
        let (code, body) = probe
            .write(
                WriteOp::Update,
                key,
                TOPIC,
                json!({
                    "holder": holder,
                    "meta": { "status": "away" },
                    "lifetime": entry.lifetime,
                    "mutation_seq": entry.mutation_seq,
                    "expected_ref": entry.phx_ref,
                }),
            )
            .await?;
        assert_eq!(code, "ok", "{key}: {body}");
        assert_eq!(body["lifetime"], entry.lifetime, "{key}: {body}");
        let (code, stale) = probe
            .write(
                WriteOp::Update,
                key,
                TOPIC,
                json!({
                    "holder": holder,
                    "meta": { "status": "busy" },
                    "lifetime": body["lifetime"],
                    "mutation_seq": body["mutation_seq"],
                    "expected_ref": entry.phx_ref,
                }),
            )
            .await?;
        assert_eq!(code, "conflict", "{key}: {stale}");
        updated.push(body);
    }
    for key in &keys {
        probe.wait_for(key, TOPIC, vec![json!("away")]).await?;
    }

    for (key, body) in keys.iter().zip(&updated) {
        let (code, reply) = probe
            .write(
                WriteOp::Untrack,
                key,
                TOPIC,
                json!({ "holder": holder, "lifetime": body["lifetime"], "mutation_seq": body["mutation_seq"] }),
            )
            .await?;
        assert_eq!(code, "ok", "{key}: {reply}");
        assert_eq!(reply["untracked"], true);
    }
    for key in &keys {
        probe.wait_for(key, TOPIC, Vec::new()).await?;
    }

    second.shutdown().await;
    first.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_for_another_holder_are_refused() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, "edge-a").await?;
    let probe = Probe::new(&server).await?;
    let owner = HolderId::generate()?;
    let intruder = HolderId::generate()?;
    let entry = probe.track("ana", TOPIC, owner).await?;

    let (code, _) = probe
        .write(
            WriteOp::Untrack,
            "ana",
            TOPIC,
            json!({ "holder": intruder, "lifetime": entry.lifetime, "mutation_seq": entry.mutation_seq }),
        )
        .await?;
    assert_eq!(code, "gone");
    let (code, _) = probe
        .write(
            WriteOp::Update,
            "ana",
            TOPIC,
            json!({
                "holder": intruder,
                "meta": { "status": "away" },
                "lifetime": entry.lifetime,
                "mutation_seq": entry.mutation_seq,
            }),
        )
        .await?;
    assert_eq!(code, "gone");

    let foreign = probe.track("bob", TOPIC, owner).await?;
    let (code, body) = probe
        .write(
            WriteOp::Update,
            "ana",
            TOPIC,
            json!({
                "holder": owner,
                "meta": { "status": "away" },
                "lifetime": foreign.lifetime,
                "mutation_seq": entry.mutation_seq,
            }),
        )
        .await?;
    assert_eq!(code, "conflict", "{body}");
    probe.wait_for("ana", TOPIC, vec![json!("online")]).await?;

    let (code, _) = probe
        .write(
            WriteOp::Untrack,
            "nobody",
            TOPIC,
            json!({ "holder": owner, "lifetime": entry.lifetime, "mutation_seq": entry.mutation_seq }),
        )
        .await?;
    assert_eq!(code, "gone");

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeats_keep_browser_holders_alive() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, "edge-a").await?;
    let probe = Probe::new(&server).await?;
    let alive = HolderId::generate()?;
    let silent = HolderId::generate()?;
    let entry = probe.track("ana", TOPIC, alive).await?;
    probe.track("bob", TOPIC, silent).await?;

    for _ in 0..7 {
        let (code, body) = probe
            .holder_op(HolderOp::Heartbeat, "ana", json!({ "entries": [entry.beat(alive)] }))
            .await?;
        assert_eq!(code, "ok", "{body}");
        assert_eq!(body["interval"], 1);
        assert_eq!(body["entries"], json!(["ok"]), "{body}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    probe.wait_for("ana", TOPIC, vec![json!("online")]).await?;
    probe.wait_for("bob", TOPIC, Vec::new()).await?;

    tokio::time::sleep(LEASE_TTL + Duration::from_secs(2)).await;
    probe.wait_for("ana", TOPIC, Vec::new()).await?;
    let (code, body) = probe
        .holder_op(HolderOp::Heartbeat, "ana", json!({ "entries": [entry.beat(alive)] }))
        .await?;
    assert_eq!(code, "ok", "{body}");
    assert_eq!(body["entries"], json!(["gone"]), "{body}");

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn release_removes_every_entry_of_a_caller() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, "edge-a").await?;
    let probe = Probe::new(&server).await?;
    let tab = HolderId::generate()?;
    let other_tab = HolderId::generate()?;
    let bystander = HolderId::generate()?;
    let lobby = probe.track("ana", TOPIC, tab).await?;
    let kitchen = probe.track("ana", "room:kitchen", tab).await?;
    let garden = probe.track("ana", "room:garden", other_tab).await?;
    probe.track("bob", TOPIC, bystander).await?;
    let target = |entry: &Entry| json!({ "topic": entry.topic, "lifetime": entry.lifetime });

    let (code, body) = probe
        .holder_op(
            HolderOp::Release,
            "ana",
            json!({ "holder": other_tab, "targets": [target(&garden)] }),
        )
        .await?;
    assert_eq!(code, "ok", "{body}");
    assert_eq!(body["released"][0]["status"], "released", "{body}");
    assert_eq!(body["holder_freed"], true, "{body}");
    probe.wait_for("ana", "room:garden", Vec::new()).await?;
    probe.wait_for("ana", TOPIC, vec![json!("online")]).await?;

    let (code, body) = probe
        .holder_op(
            HolderOp::Release,
            "ana",
            json!({ "holder": tab, "targets": [target(&lobby), target(&kitchen)] }),
        )
        .await?;
    assert_eq!(code, "ok", "{body}");
    let statuses: Vec<&Value> = body["released"]
        .as_array()
        .ok_or("release reply has no released list")?
        .iter()
        .map(|released| &released["status"])
        .collect();
    assert_eq!(statuses, [&json!("released"), &json!("released")], "{body}");
    assert_eq!(body["holder_freed"], true, "{body}");
    probe.wait_for("ana", TOPIC, Vec::new()).await?;
    probe.wait_for("ana", "room:kitchen", Vec::new()).await?;
    probe.wait_for("bob", TOPIC, vec![json!("online")]).await?;

    let (code, body) = probe
        .holder_op(
            HolderOp::Release,
            "ana",
            json!({ "holder": tab, "targets": [target(&lobby)] }),
        )
        .await?;
    assert_eq!(code, "ok", "{body}");
    assert_eq!(body["released"][0]["status"], "gone", "{body}");

    handle.shutdown().await;
    Ok(())
}
