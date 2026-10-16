//! Authenticated end to end: the provisioner creates the streams, the callout and sweep admit and
//! revoke browser clients, the service runs as the tenant's restricted runtime user, and every
//! browser client connects over the websocket listener. No part of this path disables auth.

#[path = "../../trogon_presence_auth/tests/common/mod.rs"]
mod common;
mod support;

use std::path::Path;
use std::time::Duration;

use async_nats::connection::State;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use trogon_presence::{HolderId, PresenceConfig, Presences, ShardCount, ViewCursor, WriterMode};
use trogon_presence_auth::{CalloutContext, ConnectToken, Rejection, SessionTarget, UnixSeconds, UserJwtLifetime};
use trogon_presence_service::{
    KeepaliveInterval, NodeId, PresenceReader, ReaderEvent, ReaderIdentity, ReaderOptions, ResnapshotInterval,
    ServiceConfig, ShardLeaseTtl,
};

use common::{topics, BoxError, Fixture, FixtureOptions, TestResult, DEFAULT_SESSION, TENANT_A, TENANT_B};
use support::trace::{self, Trace};
use support::{metas, Browser};

const SCENARIO: &str = "authenticated_websocket_e2e";
const TOPIC: &str = "rooms.e2e";
const CONVERGE: Duration = Duration::from_secs(10);
const TAKEOVER: Duration = Duration::from_secs(30);
const SWEEP_KICK: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(500);
const STAYS_DOWN: Duration = Duration::from_secs(2);
const SHORT_SESSION: Duration = Duration::from_secs(6);
const EXPIRY_SLACK: Duration = Duration::from_secs(5);
const MAX_PAYLOAD: usize = 1024 * 1024;

macro_rules! fixture {
    ($options:expr) => {
        match Fixture::start($options).await {
            Some(fixture) => fixture,
            None => return Ok(()),
        }
    };
}

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

fn reader_options() -> Result<ReaderOptions, BoxError> {
    Ok(ReaderOptions::default().with_resnapshot(ResnapshotInterval::try_from(Duration::from_secs(3600))?))
}

async fn read(browser: &Browser, options: ReaderOptions) -> Result<PresenceReader, BoxError> {
    let identity = ReaderIdentity::new(browser.key().clone(), browser.identity.cid);
    let topic = TOPIC.parse()?;
    Ok(PresenceReader::start(browser.client().clone(), identity, topic, options).await?)
}

async fn next_snapshot(
    events: &mut broadcast::Receiver<ReaderEvent>,
    within: Duration,
) -> Result<ViewCursor, BoxError> {
    tokio::time::timeout(within, async {
        loop {
            if let ReaderEvent::Snapshot { cursor, .. } = events.recv().await? {
                return Ok::<_, BoxError>(cursor);
            }
        }
    })
    .await
    .map_err(|_| format!("no reader snapshot within {within:?}"))?
}

async fn converge(reader: &PresenceReader, wanted: impl Fn(&Presences) -> bool) -> Result<Presences, BoxError> {
    let mut watch = reader.watch();
    tokio::time::timeout(CONVERGE, async {
        loop {
            let state = watch.borrow_and_update().clone();
            if wanted(&state) {
                return Ok::<_, BoxError>(state);
            }
            watch.changed().await?;
        }
    })
    .await
    .map_err(|_| format!("reader did not converge, holds {:?}", reader.presences()))?
}

fn statuses(state: &Presences, browser: &Browser) -> Vec<Value> {
    let mut seen: Vec<Value> = metas(state, browser.key())
        .unwrap_or_default()
        .iter()
        .map(|meta| meta["status"].clone())
        .collect();
    seen.sort_by_key(|status| status.to_string());
    seen
}

async fn authorize(fixture: &Fixture, token: &ConnectToken) -> Result<Result<(), Rejection>, BoxError> {
    let direct = fixture.direct_request(token)?;
    let context = CalloutContext {
        server_xkey: &direct.server_xkey,
        max_payload: MAX_PAYLOAD,
        now: UnixSeconds::now(),
    };
    Ok(fixture.callout.authorize(&direct.request, &context).await.map(|_| ()))
}

fn count(records: &[Value], kind: &str) -> usize {
    records.iter().filter(|record| record["kind"] == kind).count()
}

async fn scenario(golden: &Path) -> TestResult {
    let fixture = fixture!(support::websocket_only()?);
    let ws = support::ws_url(&fixture)?.to_owned();
    let sweeps = support::serve_sweeps(&fixture).await?;
    let first = support::provision_and_start(&fixture, &fixture.users.app_a, service_config("edge-a")?).await?;
    let isolated = support::provision_and_start(&fixture, &fixture.users.app_b, service_config("edge-b")?).await?;
    let granted = topics(&[TOPIC])?;
    let topic = granted[0].clone();

    let probe = fixture
        .enroller
        .enroll(&fixture.enroll_request(TENANT_A, "probe", "s1", &granted, DEFAULT_SESSION)?)
        .await?;
    let standard = fixture.token(&probe.identity)?;
    assert!(
        fixture.connect(&probe.identity, &standard).await.is_err(),
        "websocket_only_grant_admitted_a_standard_connection"
    );
    let websocket = fixture.token(&probe.identity)?;
    fixture.connect_to(&ws, &probe.identity, &websocket).await?;

    let reader = Browser::join(&fixture, TENANT_A, "bob", "s1", &granted, DEFAULT_SESSION).await?;
    let mut trace = Trace::attach(
        reader.client().clone(),
        reader.key().clone(),
        reader.identity.cid,
        topic.clone(),
        ShardCount::DEFAULT,
    )
    .await?;
    let presence = read(&reader, reader_options()?).await?;
    let mut events = presence.events();
    let twin = Browser::join(&fixture, TENANT_B, "bob", "s1", &granted, DEFAULT_SESSION).await?;
    assert_eq!(twin.key(), reader.key(), "app_b_twin_shares_the_raw_key");
    let other = read(&twin, reader_options()?).await?;
    let mut other_events = other.events();
    let initial = next_snapshot(&mut events, CONVERGE).await?;
    next_snapshot(&mut other_events, CONVERGE).await?;

    let mut writer = Browser::join(&fixture, TENANT_A, "ana", "s1", &granted, DEFAULT_SESSION).await?;
    let (web, phone) = (HolderId::generate()?, HolderId::generate()?);
    let web_entry = writer
        .track(&topic, web, json!({ "status": "online", "device": "web" }))
        .await?;
    converge(&presence, |state| statuses(state, &writer) == [json!("online")]).await?;
    let phone_entry = writer
        .track(&topic, phone, json!({ "status": "online", "device": "phone" }))
        .await?;
    let web_entry = writer
        .update(&web_entry, json!({ "status": "busy", "device": "web" }))
        .await?;
    converge(&presence, |state| {
        statuses(state, &writer) == [json!("busy"), json!("online")]
    })
    .await?;
    let beat = writer.heartbeat(&[&web_entry, &phone_entry]).await?;
    assert_eq!(beat.code, "ok", "{}", beat.body);
    assert_eq!(beat.body["entries"], json!(["ok", "ok"]), "{}", beat.body);

    first.shutdown().await;
    let second = support::start(&fixture, &fixture.users.app_a, service_config("edge-c")?).await?;
    let resnapshot = next_snapshot(&mut events, TAKEOVER).await?;
    match (initial, resnapshot) {
        (ViewCursor::Service { epoch: before, .. }, ViewCursor::Service { epoch: after, .. }) => {
            assert_ne!(before, after, "takeover_changes_the_epoch")
        }
        other => return Err(format!("reader cursors are not service cursors: {other:?}").into()),
    }
    converge(&presence, |state| {
        statuses(state, &writer) == [json!("busy"), json!("online")]
    })
    .await?;

    writer
        .update(&web_entry, json!({ "status": "away", "device": "web" }))
        .await?;
    let gone = writer.untrack(&phone_entry).await?;
    assert_eq!(gone.code, "ok", "{}", gone.body);
    assert_eq!(gone.body["untracked"], true, "{}", gone.body);
    converge(&presence, |state| statuses(state, &writer) == [json!("away")]).await?;

    trace.settle(SETTLE).await?;
    let (manifest, served) = trace.final_snapshot().await?;
    let state = presence.presences();
    assert_eq!(
        serde_json::to_value(&state)?,
        serde_json::to_value(&served)?,
        "rust_reader_matches_the_service_snapshot"
    );
    let mut snapshots = 2;
    while let Ok(event) = events.try_recv() {
        snapshots += usize::from(matches!(event, ReaderEvent::Snapshot { .. }));
    }
    let resyncs = snapshots - 1;
    assert_eq!(resyncs, 1, "forced_resnapshot_happens_once");
    let records = trace.records().to_vec();
    assert!(count(&records, "manifest") >= 2, "capture_holds_both_snapshots");
    assert!(count(&records, "snapshot-part") >= 2, "capture_holds_snapshot_parts");
    let kinds: Vec<(&str, usize)> = ["manifest", "snapshot-part", "diff", "keepalive", "epoch"]
        .into_iter()
        .map(|kind| (kind, count(&records, kind)))
        .collect();
    assert!(count(&records, "diff") >= 2, "capture_holds_the_diffs: {kinds:?}");
    assert!(count(&records, "keepalive") >= 1, "capture_holds_keepalives");
    assert!(count(&records, "epoch") >= 1, "capture_holds_the_epoch_hint");
    let capture = trace.write(SCENARIO, resyncs, &manifest, &state, golden)?;
    if let Some(replayed) = trace::replay(&capture)? {
        assert!(
            replayed.contains(&format!("ok   {SCENARIO}:")),
            "phoenix_replay_agrees_with_the_rust_reader:\n{replayed}"
        );
        assert!(
            !replayed.contains("resolved by final list"),
            "phoenix_replay_needed_the_final_list:\n{replayed}"
        );
    }

    assert!(other.presences().is_empty(), "app_b_twin_sees_no_app_a_presence");
    while let Ok(event) = other_events.try_recv() {
        assert!(
            !matches!(event, ReaderEvent::Diff(_)),
            "app_b_twin_received_an_app_a_diff: {event:?}"
        );
    }

    let target = SessionTarget {
        user: Fixture::user(TENANT_A, "ana")?,
        sid: writer.identity.sid.clone(),
    };
    assert!(fixture.commands.revoke_session(&target).await?.state_committed());
    let kicked = support::disconnected_within(&mut writer.session, SWEEP_KICK).await;
    assert!(!kicked.is_empty(), "revoked_session_is_kicked_by_the_sweep");
    tokio::time::sleep(STAYS_DOWN).await;
    assert_ne!(
        writer.client().connection_state(),
        State::Connected,
        "revoked_session_reconnected_on_its_own"
    );
    let fresh = fixture.token(&writer.identity)?;
    assert!(
        fixture.connect_to(&ws, &writer.identity, &fresh).await.is_err(),
        "revoked_session_reconnect_is_denied"
    );
    let rejection = authorize(&fixture, &fresh).await?;
    assert!(
        matches!(
            rejection,
            Err(Rejection::SessionRevoked) | Err(Rejection::EpochMismatch)
        ),
        "{rejection:?}"
    );
    assert_eq!(reader.client().connection_state(), State::Connected);

    presence.close().await;
    other.close().await;
    second.shutdown().await;
    isolated.shutdown().await;
    sweeps.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn authenticated_websocket_round_trip() -> TestResult {
    let capture = tempfile::tempdir()?;
    scenario(capture.path()).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites conformance/golden, run with `mise run presence:conformance:record`"]
async fn authenticated_websocket_e2e() -> TestResult {
    scenario(&trace::conformance_dir().join("golden")).await
}

#[tokio::test(flavor = "multi_thread")]
async fn user_jwt_expiry_disconnects_until_a_fresh_token() -> TestResult {
    let fixture = fixture!(FixtureOptions {
        user_jwt_lifetime: UserJwtLifetime::new(Duration::from_secs(60), Duration::from_secs(900))?,
        ..support::websocket_only()?
    });
    let ws = support::ws_url(&fixture)?.to_owned();
    let granted = topics(&[TOPIC])?;
    let mut browser = Browser::join(&fixture, TENANT_A, "ana", "s1", &granted, SHORT_SESSION).await?;
    let stale = fixture.token(&browser.identity)?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    browser.client().flush().await?;
    let early = support::drain_events(&mut browser.session);
    assert!(
        early.iter().all(|event| !event.to_lowercase().contains("disconnected")),
        "connection_survives_before_expiry: {early:?}"
    );
    let events = support::disconnected_within(&mut browser.session, SHORT_SESSION + EXPIRY_SLACK).await;
    assert!(!events.is_empty(), "server_disconnects_at_user_jwt_expiry");
    assert!(
        UnixSeconds::now() >= browser.identity.session_expires_at,
        "disconnect_waits_for_the_expiry: {events:?}"
    );
    tokio::time::sleep(STAYS_DOWN).await;
    assert_ne!(
        browser.client().connection_state(),
        State::Connected,
        "expired_client_reconnected_with_its_old_token"
    );
    assert!(
        fixture.connect_to(&ws, &browser.identity, &stale).await.is_err(),
        "expired_session_reconnect_is_denied"
    );
    let rejection = authorize(&fixture, &stale).await?;
    assert!(matches!(rejection, Err(Rejection::SessionExpired)), "{rejection:?}");

    let renewed = Browser::join(&fixture, TENANT_A, "ana", "s2", &granted, DEFAULT_SESSION).await?;
    renewed.client().flush().await?;
    assert_eq!(renewed.client().connection_state(), State::Connected);
    Ok(())
}
