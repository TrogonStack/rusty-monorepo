//! Browser glue harness: a small newline-delimited JSON RPC sidecar in front of the real
//! fixture, auth commands and presence service, so `node --test` can drive the real
//! `js/trogon-presence` package against the real stack instead of a mock.

#[path = "../../trogon_presence_auth/tests/common/mod.rs"]
mod common;
mod support;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use trogon_presence::{ConnectionId, PresenceConfig, ShardCount, Topic, WriterMode};
use trogon_presence_auth::{RefreshRequest, SessionTarget, UnixSeconds};
use trogon_presence_service::{KeepaliveInterval, NodeId, ServiceConfig, ServiceHandle, ShardLeaseTtl};

use common::{BoxError, Fixture, TestResult};

/// A requested topic naming this substring is never granted; the harness reports it back
/// as a denial instead of calling the real enroller for it. The real `GrantPolicy` has no
/// per-topic allow/deny surface to drive directly, so this stands in for an application
/// backend's own authorization decision.
const DENY_MARKER: &str = "denied";

fn service_config(node: &str) -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed);
    Ok(ServiceConfig::new(presence, node.parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

macro_rules! fixture {
    ($options:expr) => {
        match Fixture::start($options).await {
            Some(fixture) => fixture,
            None => return Ok(()),
        }
    };
}

fn field<'a>(request: &'a Value, name: &str) -> Result<&'a str, BoxError> {
    request[name]
        .as_str()
        .ok_or_else(|| format!("request missing {name}").into())
}

async fn dispatch(
    fixture: &Fixture,
    service: &mut Option<ServiceHandle>,
    node_generation: &mut usize,
    cmd: &str,
    request: &Value,
) -> Result<Value, BoxError> {
    match cmd {
        "enroll" => {
            let tenant = field(request, "tenant")?;
            let sub = field(request, "sub")?;
            let sid = field(request, "sid")?;
            let session_secs = request["session_secs"].as_u64().ok_or("enroll missing session_secs")?;
            let raw_topics = request["topics"]
                .as_array()
                .ok_or("enroll missing topics")?
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_owned());
            let mut granted = Vec::new();
            let mut denied = Vec::new();
            for raw in raw_topics {
                if raw.contains(DENY_MARKER) {
                    denied.push(json!({ "topic": raw, "reason": "private topic not granted" }));
                } else {
                    granted.push(raw.parse::<Topic>()?);
                }
            }
            let enroll_request =
                fixture.enroll_request(tenant, sub, sid, &granted, Duration::from_secs(session_secs))?;
            let enrolled = fixture.enroller.enroll(&enroll_request).await?;
            let token = fixture.token(&enrolled.identity)?;
            Ok(json!({
                "ok": true,
                "token": token.as_str(),
                "connection_id": enrolled.identity.cid.to_string(),
                "denied": denied,
            }))
        }
        "refresh" => {
            let tenant = field(request, "tenant")?;
            let sub = field(request, "sub")?;
            let sid = field(request, "sid")?;
            let connection_id: ConnectionId = field(request, "connection_id")?.parse()?;
            let session_secs = request["session_secs"].as_u64().ok_or("refresh missing session_secs")?;
            let refresh_request = RefreshRequest {
                session: SessionTarget {
                    user: Fixture::user(tenant, sub)?,
                    sid: sid.parse()?,
                },
                realm: fixture.realm.clone(),
                cid: connection_id,
                session_expires_at: UnixSeconds::now().saturating_add(Duration::from_secs(session_secs)),
            };
            let refreshed = fixture.enroller.refresh_session(&refresh_request).await?;
            let token = fixture.token(&refreshed.identity)?;
            Ok(json!({
                "ok": true,
                "token": token.as_str(),
                "connection_id": refreshed.identity.cid.to_string(),
            }))
        }
        "revoke" => {
            let tenant = field(request, "tenant")?;
            let sub = field(request, "sub")?;
            let sid = field(request, "sid")?;
            let target = SessionTarget {
                user: Fixture::user(tenant, sub)?,
                sid: sid.parse()?,
            };
            let outcome = fixture.commands.revoke_session(&target).await?;
            Ok(json!({ "ok": true, "committed": outcome.state_committed() }))
        }
        "takeover" => {
            *node_generation += 1;
            let node = format!("edge-{}", node_generation);
            if let Some(old) = service.take() {
                old.shutdown().await;
            }
            let fresh = support::start(fixture, &fixture.users.app_a, service_config(&node)?).await?;
            *service = Some(fresh);
            Ok(json!({ "ok": true }))
        }
        other => Err(format!("unknown command {other}").into()),
    }
}

async fn serve_connection(
    stream: TcpStream,
    fixture: &Fixture,
    service: &mut Option<ServiceHandle>,
    node_generation: &mut usize,
) -> Result<(), BoxError> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let cmd = request["cmd"].as_str().unwrap_or_default().to_owned();
        let mut body = match dispatch(fixture, service, node_generation, &cmd, &request).await {
            Ok(Value::Object(map)) => map,
            Ok(other) => {
                let mut map = Map::new();
                map.insert("ok".to_string(), Value::Bool(true));
                map.insert("value".to_string(), other);
                map
            }
            Err(error) => {
                let mut map = Map::new();
                map.insert("ok".to_string(), Value::Bool(false));
                map.insert("error".to_string(), Value::String(error.to_string()));
                map
            }
        };
        body.insert("id".to_string(), id);
        let mut bytes = serde_json::to_vec(&Value::Object(body))?;
        bytes.push(b'\n');
        writer.write_all(&bytes).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_glue() -> TestResult {
    let fixture = fixture!(support::websocket_only()?);
    let ws = support::ws_url(&fixture)?.to_owned();
    let sweeps = support::serve_sweeps(&fixture).await?;
    let mut service =
        Some(support::provision_and_start(&fixture, &fixture.users.app_a, service_config("edge-a")?).await?);
    let mut node_generation = 0usize;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let js_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../js/trogon-presence");

    let mut child = Command::new("node")
        .arg("--test")
        .arg("--test-concurrency=1")
        .arg("test/**/*.test.js")
        .current_dir(&js_dir)
        .env("PRESENCE_RPC_ADDR", addr.to_string())
        .env("PRESENCE_WS_URL", &ws)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let exit = tokio::spawn(async move { child.wait().await });
    tokio::pin!(exit);

    let outcome: Result<(), BoxError> = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if let Err(error) = serve_connection(stream, &fixture, &mut service, &mut node_generation).await {
                    break Err(error);
                }
            }
            status = &mut exit => {
                let status = status??;
                if !status.success() {
                    break Err(format!("node --test exited with {status}").into());
                }
                break Ok(());
            }
        }
    };

    if let Some(service) = service {
        service.shutdown().await;
    }
    sweeps.abort();
    outcome?;
    Ok(())
}
