#![allow(dead_code)]

pub mod trace;

use std::time::Duration;

use serde_json::{json, Value};
use trogon_presence::{HolderId, PresenceKey, ProvisionOptions, Topic};
use trogon_presence_auth::{ConnectIdentity, ConnectToken, RateBurst, RateLimit};
use trogon_presence_service::reply::HEADER_CODE;
use trogon_presence_service::subjects::HolderOp;
use trogon_presence_service::{ServiceConfig, ServiceHandle, WriteOp};

use crate::common::{BoxError, Fixture, FixtureOptions, Session, TenantUsers};

pub const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(20);
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// Websocket listener on, only `WEBSOCKET` connections admitted, and a callout rate limit
/// generous enough that client auto-reconnect attempts never mask the denial under test.
pub fn websocket_only() -> Result<FixtureOptions, BoxError> {
    Ok(FixtureOptions {
        rate_limit: RateLimit::new(Duration::from_millis(1), RateBurst::try_from(100)?)?,
        websocket: true,
        connection_types: vec!["websocket".parse()?],
        ..FixtureOptions::default()
    })
}

pub fn ws_url(fixture: &Fixture) -> Result<&str, BoxError> {
    fixture
        .ws_url
        .as_deref()
        .ok_or_else(|| "fixture has no websocket listener".into())
}

/// Spawns the sweep executor the way the auth binary's `run` does beside the callout.
pub async fn serve_sweeps(fixture: &Fixture) -> Result<tokio::task::JoinHandle<()>, BoxError> {
    let executor = fixture.executor(fixture.sweep_config(vec![fixture.server()?])?).await?;
    Ok(tokio::spawn(async move { executor.serve().await }))
}

/// Provisions the tenant streams as its provisioner, then starts the service as its runtime user.
pub async fn provision_and_start(
    fixture: &Fixture,
    tenant: &TenantUsers,
    config: ServiceConfig,
) -> Result<ServiceHandle, BoxError> {
    let provisioner = tenant.provisioner.connect(&fixture.url).await?;
    trogon_presence_service::provision(provisioner, &config, ProvisionOptions::default()).await?;
    start(fixture, tenant, config).await
}

pub async fn start(fixture: &Fixture, tenant: &TenantUsers, config: ServiceConfig) -> Result<ServiceHandle, BoxError> {
    let expected = usize::from(config.presence().shards().get());
    let runtime = tenant.runtime.connect(&fixture.url).await?;
    let handle = trogon_presence_service::start(runtime, config).await?;
    let owned = tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != expected || handle.writer_shards().len() != expected {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    if owned.is_err() {
        let held = (handle.owned_shards().len(), handle.writer_shards().len());
        handle.shutdown().await;
        return Err(format!("service held {held:?} view and writer shards, expected {expected}").into());
    }
    Ok(handle)
}

pub struct Browser {
    pub identity: ConnectIdentity,
    pub token: ConnectToken,
    pub session: Session,
}

pub struct Reply {
    pub code: String,
    pub body: Value,
}

impl Browser {
    /// Enrolls a session, mints its connect token and connects over the websocket listener.
    pub async fn join(
        fixture: &Fixture,
        tenant: &str,
        sub: &str,
        sid: &str,
        topics: &[Topic],
        session: Duration,
    ) -> Result<Self, BoxError> {
        let request = fixture.enroll_request(tenant, sub, sid, topics, session)?;
        let enrolled = fixture.enroller.enroll(&request).await?;
        let token = fixture.token(&enrolled.identity)?;
        let session = fixture.connect_to(ws_url(fixture)?, &enrolled.identity, &token).await?;
        Ok(Self {
            identity: enrolled.identity,
            token,
            session,
        })
    }

    pub fn key(&self) -> &PresenceKey {
        self.identity.sub.key()
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.session.client
    }

    pub async fn command(&self, subject: String, body: &Value) -> Result<Reply, BoxError> {
        let reply = tokio::time::timeout(
            REPLY_TIMEOUT,
            self.session.client.request(subject, serde_json::to_vec(body)?.into()),
        )
        .await
        .map_err(|_| "no command reply")??;
        let code = reply
            .headers
            .as_ref()
            .and_then(|headers| headers.get(HEADER_CODE))
            .map(|value| value.as_str().to_owned())
            .ok_or("reply has no code")?;
        Ok(Reply {
            code,
            body: serde_json::from_slice(&reply.payload)?,
        })
    }
}

pub async fn disconnected_within(session: &mut Session, within: Duration) -> Vec<String> {
    let until = tokio::time::Instant::now() + within;
    let mut seen = Vec::new();
    loop {
        match tokio::time::timeout_at(until, session.events.recv()).await {
            Ok(Some(event)) => {
                let disconnected = event.to_lowercase().contains("disconnected");
                seen.push(event);
                if disconnected {
                    return seen;
                }
            }
            Ok(None) | Err(_) => return Vec::new(),
        }
    }
}

pub fn drain_events(session: &mut Session) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(event) = session.events.try_recv() {
        seen.push(event);
    }
    seen
}

/// One tracked entry as the writer sees it: the holder plus the fence the service returned.
#[derive(Debug, Clone)]
pub struct Entry {
    pub holder: HolderId,
    pub topic: Topic,
    pub lifetime: Value,
    pub mutation_seq: Value,
}

impl Entry {
    fn from_reply(holder: HolderId, topic: &Topic, reply: &Reply) -> Result<Self, BoxError> {
        if reply.code != "ok" {
            return Err(format!("write replied {}: {}", reply.code, reply.body).into());
        }
        Ok(Self {
            holder,
            topic: topic.clone(),
            lifetime: reply.body["lifetime"].clone(),
            mutation_seq: reply.body["mutation_seq"].clone(),
        })
    }

    fn fence(&self) -> Value {
        json!({ "holder": self.holder, "lifetime": self.lifetime, "mutation_seq": self.mutation_seq })
    }

    pub fn beat(&self) -> Value {
        let mut beat = self.fence();
        beat["topic"] = json!(self.topic.as_str());
        beat
    }
}

impl Browser {
    pub async fn track_raw(&self, topic: &Topic, holder: HolderId, meta: Value) -> Result<Reply, BoxError> {
        self.command(
            WriteOp::Track.subject(self.key(), topic),
            &json!({ "holder": holder, "meta": meta }),
        )
        .await
    }

    pub async fn track(&self, topic: &Topic, holder: HolderId, meta: Value) -> Result<Entry, BoxError> {
        Entry::from_reply(holder, topic, &self.track_raw(topic, holder, meta).await?)
    }

    pub async fn update(&self, entry: &Entry, meta: Value) -> Result<Entry, BoxError> {
        let mut body = entry.fence();
        body["meta"] = meta;
        let reply = self
            .command(WriteOp::Update.subject(self.key(), &entry.topic), &body)
            .await?;
        Entry::from_reply(entry.holder, &entry.topic, &reply)
    }

    pub async fn untrack(&self, entry: &Entry) -> Result<Reply, BoxError> {
        self.command(WriteOp::Untrack.subject(self.key(), &entry.topic), &entry.fence())
            .await
    }

    pub async fn heartbeat(&self, entries: &[&Entry]) -> Result<Reply, BoxError> {
        let beats: Vec<Value> = entries.iter().map(|entry| entry.beat()).collect();
        self.command(HolderOp::Heartbeat.subject(self.key()), &json!({ "entries": beats }))
            .await
    }

    pub async fn release(&self, holder: HolderId, entries: &[&Entry]) -> Result<Reply, BoxError> {
        let targets: Vec<Value> = entries
            .iter()
            .map(|entry| json!({ "topic": entry.topic.as_str(), "lifetime": entry.lifetime }))
            .collect();
        self.command(
            HolderOp::Release.subject(self.key()),
            &json!({ "holder": holder, "targets": targets }),
        )
        .await
    }
}

/// The metas a presence state lists for one raw key, as JSON.
pub fn metas(state: &trogon_presence::Presences, key: &PresenceKey) -> Result<Vec<Value>, BoxError> {
    let state = serde_json::to_value(state)?;
    Ok(state[key.to_string()]["metas"].as_array().cloned().unwrap_or_default())
}
