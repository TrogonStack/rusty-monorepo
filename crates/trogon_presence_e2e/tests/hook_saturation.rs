//! Liveness under hook saturation. Lives in this crate rather than in the service tests because
//! it needs the authenticated fixture (callout admitted websocket clients, restricted runtime
//! user) together with the hook runtime and the service, and only this crate depends on all three.
//! A slow guest pins the runtime's single running call and its single queued call while
//! heartbeats, releases and the lease sweep must keep working on their own deadlines.

#[path = "../../trogon_presence_auth/tests/common/mod.rs"]
mod common;
#[path = "../../trogon_presence_hooks/tests/support/guest.rs"]
mod guest;
mod support;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;
use trogon_presence::{
    HeartbeatInterval, HolderId, LeaseTtl, MarkerTtl, PresenceConfig, Presences, ShardCount, WriterMode,
};
use trogon_presence_hooks::{HookConfig, HookPolicy};
use trogon_presence_service::{
    KeepaliveInterval, NodeId, PresenceReader, ReaderIdentity, ReaderOptions, ServiceConfig, ShardLeaseTtl,
};

use common::{topics, BoxError, Fixture, TestResult, DEFAULT_SESSION, TENANT_A};
use support::{metas, Browser};

const TOPIC: &str = "rooms.saturated";
const LEASE_TTL: Duration = Duration::from_secs(3);
const HEARTBEAT: Duration = Duration::from_secs(1);
const WINDOW: Duration = Duration::from_secs(8);
const LATENCY_BOUND: Duration = Duration::from_millis(500);
const FLOOD_WORKERS: usize = 3;
const FLOOD_BACKOFF: Duration = Duration::from_millis(20);
const SAMPLE: Duration = Duration::from_millis(50);
const CONVERGE: Duration = Duration::from_secs(10);

fn service_config(component: std::path::PathBuf) -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::new(
        Default::default(),
        LeaseTtl::try_from(LEASE_TTL)?,
        HeartbeatInterval::try_from(HEARTBEAT)?,
        MarkerTtl::try_from(LEASE_TTL * 2)?,
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed);
    let hook = HookConfig {
        policy: HookPolicy::FailClosed,
        ..HookConfig::new(component)
    };
    Ok(ServiceConfig::new(presence, "edge-saturated".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?)
        .with_hook(hook))
}

fn holds(state: &Presences, browser: &Browser) -> bool {
    metas(state, browser.key()).is_ok_and(|metas| !metas.is_empty())
}

#[tokio::test(flavor = "multi_thread")]
async fn heartbeat_and_release_stay_live_while_hooks_saturate() -> TestResult {
    let Some(component) = guest::example_component() else {
        return Ok(());
    };
    let Some(fixture) = Fixture::start(support::websocket_only()?).await else {
        return Ok(());
    };
    let service = support::provision_and_start(&fixture, &fixture.users.app_a, service_config(component)?).await?;
    let granted = topics(&[TOPIC])?;
    let topic = granted[0].clone();

    let ana = Browser::join(&fixture, TENANT_A, "ana", "s1", &granted, DEFAULT_SESSION).await?;
    let bob = Browser::join(&fixture, TENANT_A, "bob", "s1", &granted, DEFAULT_SESSION).await?;
    let carol = Browser::join(&fixture, TENANT_A, "carol", "s1", &granted, DEFAULT_SESSION).await?;
    let slow = Arc::new(Browser::join(&fixture, TENANT_A, "slow", "s1", &granted, DEFAULT_SESSION).await?);
    let watcher = Browser::join(&fixture, TENANT_A, "watcher", "s1", &granted, DEFAULT_SESSION).await?;

    let reader = PresenceReader::start(
        watcher.client().clone(),
        ReaderIdentity::new(watcher.key().clone(), watcher.identity.cid),
        topic.clone(),
        ReaderOptions::default(),
    )
    .await?;

    let ana_entry = ana
        .track(&topic, HolderId::generate()?, json!({ "status": "online" }))
        .await?;
    bob.track(&topic, HolderId::generate()?, json!({ "status": "online" }))
        .await?;
    let carol_holder = HolderId::generate()?;
    let carol_entry = carol.track(&topic, carol_holder, json!({ "status": "online" })).await?;
    let mut watch = reader.watch();
    tokio::time::timeout(CONVERGE, async {
        loop {
            let state = watch.borrow_and_update().clone();
            if holds(&state, &ana) && holds(&state, &bob) && holds(&state, &carol) {
                return Ok::<_, BoxError>(());
            }
            watch.changed().await?;
        }
    })
    .await
    .map_err(|_| "reader never saw every holder")??;

    let flooding = Arc::new(AtomicBool::new(true));
    let mut flood = Vec::new();
    for _ in 0..FLOOD_WORKERS {
        let (slow, flooding, topic) = (slow.clone(), flooding.clone(), topic.clone());
        flood.push(tokio::spawn(async move {
            let mut codes: BTreeMap<String, usize> = BTreeMap::new();
            while flooding.load(Ordering::Relaxed) {
                let code = match HolderId::generate() {
                    Ok(holder) => match slow.track_raw(&topic, holder, json!({ "status": "online" })).await {
                        Ok(reply) => reply.code,
                        Err(_) => "no_reply".to_owned(),
                    },
                    Err(_) => "no_holder".to_owned(),
                };
                if !code.starts_with("hook_") {
                    tokio::time::sleep(FLOOD_BACKOFF).await;
                }
                *codes.entry(code).or_default() += 1;
            }
            codes
        }));
    }

    let started = Instant::now();
    let mut worst_beat = Duration::ZERO;
    let mut beating_lost = false;
    let mut bob_left = None;
    let mut next_beat = started;
    while started.elapsed() < WINDOW {
        if Instant::now() >= next_beat {
            let sent = Instant::now();
            for (holder, entry) in [(&ana, &ana_entry), (&carol, &carol_entry)] {
                let beat_sent = Instant::now();
                let beat = holder.heartbeat(&[entry]).await?;
                worst_beat = worst_beat.max(beat_sent.elapsed());
                assert_eq!(beat.code, "ok", "{}", beat.body);
                assert_eq!(beat.body["entries"], json!(["ok"]), "{}", beat.body);
            }
            next_beat = sent + HEARTBEAT;
        }
        let state = reader.presences();
        beating_lost |= !holds(&state, &ana) || !holds(&state, &carol);
        if bob_left.is_none() && !holds(&state, &bob) {
            bob_left = Some(started.elapsed());
        }
        tokio::time::sleep(SAMPLE).await;
    }

    let sent = Instant::now();
    let released = carol.release(carol_holder, &[&carol_entry]).await?;
    let release_latency = sent.elapsed();

    flooding.store(false, Ordering::Relaxed);
    let mut codes: BTreeMap<String, usize> = BTreeMap::new();
    for worker in flood {
        for (code, seen) in worker.await? {
            *codes.entry(code).or_default() += seen;
        }
    }
    eprintln!(
        "hook saturation: worst heartbeat {worst_beat:?}, release {release_latency:?}, bob left after {bob_left:?}, flood codes {codes:?}"
    );

    assert!(
        worst_beat < LATENCY_BOUND,
        "heartbeat_latency_stays_bounded: {worst_beat:?}"
    );
    assert!(
        release_latency < LATENCY_BOUND,
        "release_latency_stays_bounded: {release_latency:?}"
    );
    assert_eq!(released.code, "ok", "{}", released.body);
    assert_eq!(released.body["released"][0]["status"], "released", "{}", released.body);
    assert!(!beating_lost, "heartbeated_entry_never_expires_under_saturation");
    assert!(
        bob_left.is_some(),
        "liveness_sweep_expires_the_silent_holder_under_saturation"
    );
    assert!(!codes.contains_key("ok"), "saturated_hook_admitted_a_track: {codes:?}");
    assert!(
        codes.get("hook_unavailable").is_some_and(|seen| *seen > 0),
        "saturated_hook_rejects_or_times_out: {codes:?}"
    );

    reader.close().await;
    service.shutdown().await;
    Ok(())
}
