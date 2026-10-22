mod common;

use std::collections::BTreeSet;
use std::error::Error;
use std::process::{Command, Output};
use std::time::Duration;

use async_nats::jetstream::stream::StorageType;
use async_nats::Message;
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::{
    BucketField, DriftSeverity, FieldStatus, HeartbeatInterval, HolderId, LeaseTtl, MarkerTtl, PresenceConfig,
    PresenceKey, ProvisionOptions, Replicas, ShardCount, Topic,
};
use trogon_presence_service::admin::{
    render, Admin, AdminError, ApplyAction, ExpiryStatus, LeaseBucketCheck, LeaseState, OutputFormat,
};
use trogon_presence_service::reply::HEADER_KIND;
use trogon_presence_service::subjects::diff_subject;
use trogon_presence_service::{
    KeepaliveInterval, NodeId, ReadOp, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
};

use common::NatsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const TOPIC: &str = "room:lobby";
const LEASE_TTL: Duration = Duration::from_secs(3);
const SHARD_LEASE_TTL: Duration = Duration::from_secs(3);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const WIDER_SHARDS: u16 = 128;

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn presence_config(shards: ShardCount) -> Result<PresenceConfig, BoxError> {
    Ok(PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(LEASE_TTL)?,
        HeartbeatInterval::try_from(Duration::from_secs(1))?,
        MarkerTtl::try_from(LEASE_TTL * 2)?,
        shards,
    )?)
}

fn service_config_with(node: &str, shards: ShardCount, shard_lease: Duration) -> Result<ServiceConfig, BoxError> {
    Ok(ServiceConfig::new(presence_config(shards)?, node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(shard_lease)?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    service_config_with(node, ShardCount::DEFAULT, SHARD_LEASE_TTL)
}

async fn admin(server: &NatsServer) -> Result<Admin, BoxError> {
    Ok(Admin::new(
        server.client().await,
        service_config("admin")?,
        ProvisionOptions::default(),
    ))
}

fn shard_total() -> usize {
    usize::from(ShardCount::DEFAULT.get())
}

async fn wait_until<F>(within: Duration, what: &str, mut done: F) -> Result<(), BoxError>
where
    F: FnMut() -> bool,
{
    tokio::time::timeout(within, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for {what}").into())
}

async fn start_owning_all(server: &NatsServer, node: &str) -> Result<ServiceHandle, BoxError> {
    let handle = start_instance(server, node).await?;
    wait_until(OWNERSHIP_TIMEOUT, "view shards", || {
        handle.owned_shards().len() == shard_total()
    })
    .await?;
    common::wait_for_writers(&handle, shard_total(), OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

async fn start_instance(server: &NatsServer, node: &str) -> Result<ServiceHandle, BoxError> {
    admin(server).await?.bucket_apply().await?;
    Ok(trogon_presence_service::start(server.client().await, service_config(node)?).await?)
}

async fn track(client: &async_nats::Client, key: &str, topic: &str, holder: HolderId) -> Result<Value, BoxError> {
    let key: PresenceKey = key.parse()?;
    let topic: Topic = topic.parse()?;
    let reply = common::command(
        client,
        WriteOp::Track.subject(&key, &topic),
        &key,
        &json!({ "holder": holder, "meta": { "status": "online" } }),
    )
    .await?;
    let body = common::body_of(&reply)?;
    if common::code_of(&reply) != Some("ok") {
        return Err(format!("track {key} on {topic} replied {body}").into());
    }
    Ok(body)
}

async fn listed_keys(client: &async_nats::Client, topic: &str) -> Result<Option<BTreeSet<String>>, BoxError> {
    let caller: PresenceKey = "observer".parse()?;
    let topic: Topic = topic.parse()?;
    let reply = client
        .request(
            ReadOp::List.subject(ShardCount::DEFAULT, &caller, &topic),
            Vec::new().into(),
        )
        .await?;
    if common::code_of(&reply) != Some("ok") {
        return Ok(None);
    }
    let state = common::body_of(&reply)?;
    Ok(state.as_object().map(|map| {
        map.iter()
            .filter(|(_, value)| value["metas"].as_array().is_some_and(|metas| !metas.is_empty()))
            .map(|(key, _)| key.clone())
            .collect()
    }))
}

async fn wait_for_listed(client: &async_nats::Client, topic: &str, expected: &[&str]) -> Result<(), BoxError> {
    let expected: BTreeSet<String> = expected.iter().map(|key| (*key).to_owned()).collect();
    let deadline = tokio::time::Instant::now() + CONVERGE_TIMEOUT;
    loop {
        let seen = listed_keys(client, topic).await?;
        if seen.as_ref() == Some(&expected) {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            return Err(format!("{topic} listed {seen:?}, expected {expected:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn next_leaves(frames: &mut async_nats::Subscriber) -> Result<BTreeSet<String>, BoxError> {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let message: Message = frames.next().await.ok_or("frame subscription ended")?;
            let kind = message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(HEADER_KIND))
                .map(|value| value.as_str().to_owned());
            if kind.as_deref() != Some("diff") {
                continue;
            }
            let body: Value = serde_json::from_slice(&message.payload)?;
            let leaves: BTreeSet<String> = body["leaves"]
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            if !leaves.is_empty() {
                return Ok::<_, BoxError>(leaves);
            }
        }
    })
    .await
    .map_err(|_| "no leave diff arrived")?
}

fn cli(server: &NatsServer, args: &[&str]) -> Result<Output, BoxError> {
    Ok(Command::new(env!("CARGO_BIN_EXE_trogon-presence"))
        .args(["--nats-url", server.url(), "--log-level", "error"])
        .args(["--lease-ttl", "3s", "--marker-ttl", "6s", "--heartbeat-interval", "1s"])
        .args(["--shard-lease-ttl", "3s"])
        .args(args)
        .output()?)
}

#[tokio::test(flavor = "multi_thread")]
async fn bucket_apply_creates_then_verifies_and_refuses_drift() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;

    let created = admin(&server).await?.bucket_apply().await?;
    assert_eq!(created.data.action, ApplyAction::Created);
    assert_eq!(created.lease.action, ApplyAction::Created);

    let verified = admin(&server).await?.bucket_apply().await?;
    assert_eq!(verified.data.action, ApplyAction::Verified);
    assert_eq!(verified.lease.action, ApplyAction::Verified);

    let in_memory = ProvisionOptions {
        storage: StorageType::Memory,
        replicas: Replicas::default(),
    };
    let refused = Admin::new(client.clone(), service_config("admin")?, in_memory)
        .bucket_apply()
        .await;
    assert!(
        matches!(
            refused,
            Err(AdminError::Refused {
                field: BucketField::Storage,
                ..
            })
        ),
        "{refused:?}"
    );

    let wider = service_config_with("admin", ShardCount::try_from(WIDER_SHARDS)?, SHARD_LEASE_TTL)?;
    let refused = Admin::new(client.clone(), wider, ProvisionOptions::default())
        .bucket_apply()
        .await;
    assert!(
        matches!(
            refused,
            Err(AdminError::Refused {
                field: BucketField::ShardCount,
                ..
            })
        ),
        "{refused:?}"
    );

    let longer_leases = service_config_with("admin", ShardCount::DEFAULT, SHARD_LEASE_TTL * 20)?;
    let refused = Admin::new(client.clone(), longer_leases, ProvisionOptions::default())
        .bucket_apply()
        .await;
    assert!(
        matches!(
            refused,
            Err(AdminError::Refused {
                field: BucketField::MaxAge,
                ..
            })
        ),
        "{refused:?}"
    );

    let report = admin(&server).await?.config_check().await?;
    assert!(
        !report.has_hard_violation(),
        "refused applies must not mutate: {report:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn config_check_reports_each_field_and_flags_drift() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    admin(&server).await?.bucket_apply().await?;

    let report = admin(&server).await?.config_check().await?;
    assert!(!report.has_hard_violation(), "{report:?}");
    assert!(report.data.checks.iter().all(|check| check.status == FieldStatus::Ok));
    let LeaseBucketCheck::Checked(lease) = &report.lease else {
        return Err("lease bucket reported missing after apply".into());
    };
    assert!(lease.checks.iter().all(|check| check.status == FieldStatus::Ok));
    let table = render(&report, OutputFormat::Table)?;
    assert!(table.starts_with("stream"), "{table}");
    let json: Value = serde_json::from_str(&render(&report, OutputFormat::Json)?)?;
    assert_eq!(json["data"]["checks"][0]["status"], "ok", "{json}");

    let wider = service_config_with("admin", ShardCount::try_from(WIDER_SHARDS)?, SHARD_LEASE_TTL)?;
    let drifted = Admin::new(client.clone(), wider, ProvisionOptions::default())
        .config_check()
        .await?;
    assert!(drifted.has_hard_violation());
    let shard_check = drifted
        .data
        .checks
        .iter()
        .find(|check| check.field == BucketField::ShardCount)
        .ok_or("no shard count check")?;
    assert_eq!(shard_check.status, FieldStatus::Drift);
    assert_eq!(shard_check.severity, DriftSeverity::Hard);

    let in_memory = ProvisionOptions {
        storage: StorageType::Memory,
        replicas: Replicas::default(),
    };
    let placement = Admin::new(client.clone(), service_config("admin")?, in_memory)
        .config_check()
        .await?;
    assert!(!placement.has_hard_violation(), "placement drift is advisory");
    assert_eq!(placement.data.any_drift(), Some(BucketField::Storage));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_config_check_exits_non_zero_on_a_hard_violation() -> TestResult {
    let server = server_or_skip!();

    let applied = cli(&server, &["bucket", "apply", "--json"])?;
    assert!(applied.status.success(), "{}", String::from_utf8_lossy(&applied.stderr));
    let applied: Value = serde_json::from_slice(&applied.stdout)?;
    assert_eq!(applied["data"]["action"], "created", "{applied}");

    let checked = cli(&server, &["config", "check"])?;
    assert!(checked.status.success(), "{}", String::from_utf8_lossy(&checked.stderr));
    assert!(String::from_utf8(checked.stdout)?.contains("ok"));

    let drifted = cli(&server, &["config", "check", "--shards", "128", "--json"])?;
    assert!(!drifted.status.success());
    let drifted: Value = serde_json::from_slice(&drifted.stdout)?;
    let statuses: Vec<&Value> = drifted["data"]["checks"]
        .as_array()
        .ok_or("no checks")?
        .iter()
        .filter(|check| check["field"] == "shard count")
        .map(|check| &check["status"])
        .collect();
    assert_eq!(statuses, [&json!("drift")], "{drifted}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_and_count_read_stored_entries() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, "edge-a").await?;
    let client = server.client().await;
    let ana = HolderId::generate()?;
    let bob = HolderId::generate()?;
    track(&client, "ana", TOPIC, ana).await?;
    track(&client, "ana", "room:kitchen", ana).await?;
    track(&client, "bob", TOPIC, bob).await?;
    let admin = admin(&server).await?;
    let topic: Topic = TOPIC.parse()?;

    let all = admin.inspect(topic.clone(), None).await?;
    assert_eq!(all.unreadable, 0);
    let holders: BTreeSet<HolderId> = all.entries.iter().map(|entry| entry.holder).collect();
    assert_eq!(holders, BTreeSet::from([ana, bob]));
    assert!(all.entries.iter().all(|entry| entry.topic == topic));
    assert!(all.entries.iter().all(|entry| entry.expires_at.is_some()));
    let table = render(&all, OutputFormat::Table)?;
    assert_eq!(table.lines().count(), 3, "{table}");

    let one = admin.inspect(topic.clone(), Some("ana".parse()?)).await?;
    assert_eq!(one.entries.len(), 1);
    assert_eq!(one.entries[0].holder, ana);
    assert_eq!(
        serde_json::to_value(&one.entries[0].meta)?,
        json!({ "status": "online" })
    );

    let count = admin.count(topic.clone()).await?;
    assert_eq!((count.keys, count.metas), (2, 2));
    let json: Value = serde_json::from_str(&render(&count, OutputFormat::Json)?)?;
    assert_eq!(json, json!({ "topic": TOPIC, "keys": 2, "metas": 2 }));

    let empty = admin.count("room:empty".parse()?).await?;
    assert_eq!((empty.keys, empty.metas), (0, 0));

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shards_report_the_running_owner() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, "edge-a").await?;

    let report = admin(&server).await?.shards().await?;
    assert_eq!(report.shards.len(), shard_total());
    for shard in &report.shards {
        for lease in [shard.view, shard.writer] {
            assert!(
                matches!(lease, LeaseState::Held { owner, current_generation: true, .. } if owner == handle.owner()),
                "{shard:?}"
            );
        }
    }
    let table = render(&report, OutputFormat::Table)?;
    assert_eq!(table.lines().count(), shard_total() + 1);

    handle.shutdown().await;
    let released = admin(&server).await?.shards().await?;
    assert!(released
        .shards
        .iter()
        .all(|shard| shard.view == LeaseState::Free && shard.writer == LeaseState::Free));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn expire_announces_a_leave_without_kicking_the_connection() -> TestResult {
    let server = server_or_skip!();
    let handle = start_owning_all(&server, "edge-a").await?;
    let connection = server.client().await;
    let topic: Topic = TOPIC.parse()?;
    let mut frames = connection.subscribe(diff_subject(&topic)).await?;
    connection.flush().await?;
    let tab = HolderId::generate()?;
    let bystander = HolderId::generate()?;
    track(&connection, "ana", TOPIC, tab).await?;
    track(&connection, "ana", "room:kitchen", tab).await?;
    track(&connection, "bob", TOPIC, bystander).await?;
    wait_for_listed(&connection, TOPIC, &["ana", "bob"]).await?;

    let report = admin(&server).await?.expire(tab).await?;
    assert_eq!(report.holder, tab);
    assert_eq!(report.keys.len(), 1);
    let expired = &report.keys[0];
    assert_eq!(expired.key, "ana".parse::<PresenceKey>()?);
    assert!(expired.holder_freed);
    assert_eq!(expired.entries.len(), 2);
    assert!(expired
        .entries
        .iter()
        .all(|entry| entry.status == ExpiryStatus::Released));

    assert_eq!(next_leaves(&mut frames).await?, BTreeSet::from(["ana".to_owned()]));
    wait_for_listed(&connection, TOPIC, &["bob"]).await?;
    wait_for_listed(&connection, "room:kitchen", &[]).await?;

    assert_eq!(connection.connection_state(), async_nats::connection::State::Connected);
    track(&connection, "ana", TOPIC, tab).await?;
    wait_for_listed(&connection, TOPIC, &["ana", "bob"]).await?;

    let again = admin(&server).await?.expire(bystander).await?;
    assert_eq!(again.keys.len(), 1);
    let nothing = admin(&server).await?.expire(HolderId::generate()?).await?;
    assert!(nothing.keys.is_empty());

    handle.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn drain_hands_every_lease_to_a_peer() -> TestResult {
    let server = server_or_skip!();
    let first = start_owning_all(&server, "edge-a").await?;
    let second = start_instance(&server, "edge-b").await?;
    let admin = admin(&server).await?;

    let reply = admin.drain(first.owner()).await?;
    assert_eq!(reply.instance, first.owner());
    assert!(!reply.already_draining);
    assert_eq!(reply.released_views, shard_total());
    assert_eq!(reply.released_writers, shard_total());
    let json: Value = serde_json::from_str(&render(&reply, OutputFormat::Json)?)?;
    assert_eq!(json["released_views"], shard_total());

    wait_until(OWNERSHIP_TIMEOUT, "the peer to own every view shard", || {
        second.owned_shards().len() == shard_total()
    })
    .await?;
    common::wait_for_writers(&second, shard_total(), OWNERSHIP_TIMEOUT).await?;
    assert!(first.owned_shards().is_empty());
    assert!(first.writer_shards().is_empty());

    let report = admin.shards().await?;
    assert!(report.shards.iter().all(|shard| {
        [shard.view, shard.writer]
            .into_iter()
            .all(|lease| matches!(lease, LeaseState::Held { owner, .. } if owner == second.owner()))
    }));

    let repeated = admin.drain(first.owner()).await?;
    assert!(repeated.already_draining);

    first.shutdown().await;
    second.shutdown().await;
    Ok(())
}
