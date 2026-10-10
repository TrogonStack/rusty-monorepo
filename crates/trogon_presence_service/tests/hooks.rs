mod common;
#[path = "../../trogon_presence_hooks/tests/support/guest.rs"]
mod guest;

use std::error::Error;
use std::time::Duration;

use async_nats::{HeaderMap, Message, Subscriber};
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::{HolderId, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic, WriterMode};
use trogon_presence_hooks::{HookConfig, HookPolicy};
use trogon_presence_service::reply::{HEADER_CODE, HEADER_KIND};
use trogon_presence_service::{KeepaliveInterval, NodeId, ServiceConfig, ShardLeaseTtl, WriteOp};

use common::NatsServer;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const TOPIC: &str = "room:hooks";
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);

fn service_config(component: std::path::PathBuf) -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed);
    let hook = HookConfig {
        policy: HookPolicy::FailClosed,
        ..HookConfig::new(component)
    };
    Ok(ServiceConfig::new(presence, "edge-hooks".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?)
        .with_hook(hook))
}

fn header<'a>(headers: Option<&'a HeaderMap>, name: &str) -> Option<&'a str> {
    headers
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

async fn next_diff(frames: &mut Subscriber) -> Result<Message, BoxError> {
    tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            let message = frames.next().await.ok_or("frame subscription ended")?;
            if header(message.headers.as_ref(), HEADER_KIND) == Some("diff") {
                return Ok::<_, BoxError>(message);
            }
        }
    })
    .await
    .map_err(|_| format!("no diff frame within {FRAME_TIMEOUT:?}"))?
}

async fn track(client: &async_nats::Client, key: &str, topic: &Topic) -> Result<Message, BoxError> {
    let key: PresenceKey = key.parse()?;
    let body = json!({ "holder": HolderId::generate()?, "meta": { "status": "busy" } });
    common::command(client, WriteOp::Track.subject(&key, topic), &key, &body).await
}

#[tokio::test(flavor = "multi_thread")]
async fn track_runs_the_enrich_hook() -> TestResult {
    let Some(component) = guest::example_component() else {
        return Ok(());
    };
    let Some(server) = NatsServer::start().await else {
        return Ok(());
    };
    let config = service_config(component)?;
    let client = server.client().await;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client.clone(), config).await?;
    let shards = usize::from(ShardCount::DEFAULT.get());
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != shards {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "service did not take every shard")?;
    common::wait_for_writers(&handle, shards, OWNERSHIP_TIMEOUT).await?;

    let topic: Topic = TOPIC.parse()?;
    let mut frames = client
        .subscribe(trogon_presence_service::subjects::diff_subject(&topic))
        .await?;
    client.flush().await?;

    let tracked = track(&client, "carol", &topic).await?;
    assert_eq!(header(tracked.headers.as_ref(), HEADER_CODE), Some("ok"));
    let diff: Value = serde_json::from_slice(&next_diff(&mut frames).await?.payload)?;
    let metas = diff["joins"]["carol"]["metas"]
        .as_array()
        .ok_or_else(|| format!("no metas for carol in {diff}"))?;
    assert_eq!(metas.len(), 1, "{diff}");
    assert_eq!(metas[0]["status"], "busy");
    assert_eq!(metas[0]["enriched"], true);

    let blocked = track(&client, "blocked", &topic).await?;
    assert_eq!(header(blocked.headers.as_ref(), HEADER_CODE), Some("hook_rejected"));
    let body: Value = serde_json::from_slice(&blocked.payload)?;
    assert_eq!(body["error"], "hook_rejected");

    handle.shutdown().await;
    Ok(())
}
