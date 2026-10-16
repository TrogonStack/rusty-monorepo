mod common;

use std::time::Duration;

use async_nats::header::{NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, NATS_MESSAGE_TTL};
use async_nats::jetstream;
use async_nats::HeaderMap;
use common::NatsServer;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use trogon_presence::value::StoredValue;
use trogon_presence::{
    BarrierError, BucketName, CoalesceWindow, CursorStep, Diff, EntryKey, EntryRevision, FetchEntry, FetchError,
    HeartbeatInterval, HolderId, KvKey, LeaseTtl, MarkerTtl, Meta, MetaEntry, MetaFetcher, Presence, PresenceConfig,
    PresenceKey, ProvisionOptions, ReadBarrier, ShardCount, StreamGeneration, Topic, TopicWatch, UnixMillis, ViewDiff,
    ViewRef, ViewSnapshot, WriteOutcome, WriteReceipt,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIFF_TIMEOUT: Duration = Duration::from_secs(2);
const QUIET_PERIOD: Duration = Duration::from_millis(400);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn meta(status: &str) -> Result<Meta, serde_json::Error> {
    serde_json::from_value(serde_json::json!({ "status": status }))
}

fn lobby() -> Result<(Topic, PresenceKey), BoxError> {
    Ok(("room:lobby".parse()?, "ana@x.io".parse()?))
}

async fn next_view_diff(diffs: &mut broadcast::Receiver<ViewDiff>, within: Duration) -> Result<ViewDiff, BoxError> {
    Ok(tokio::time::timeout(within, diffs.recv()).await??)
}

async fn next_diff(diffs: &mut broadcast::Receiver<ViewDiff>, within: Duration) -> Result<Diff, BoxError> {
    Ok(next_view_diff(diffs, within).await?.into_diff())
}

async fn assert_quiet(diffs: &mut broadcast::Receiver<ViewDiff>) {
    if let Ok(diff) = tokio::time::timeout(QUIET_PERIOD, diffs.recv()).await {
        panic!("expected no further diff, got {diff:?}");
    }
}

fn applied(outcome: WriteOutcome) -> WriteReceipt {
    match outcome {
        WriteOutcome::Applied(receipt) => receipt,
        other => panic!("expected an applied write, got {other:?}"),
    }
}

fn foreign_value(topic: &Topic, key: &PresenceKey, status: &str) -> Result<StoredValue, BoxError> {
    let json = serde_json::json!({
        "v": 2,
        "topic": topic,
        "key": key,
        "phx_ref": "Fq1",
        "phx_ref_prev": null,
        "meta": { "status": status },
        "lifetime": "AAAAAAAAAAAAAAAAAAAAAA",
        "mutation_seq": "1",
        "last_op": { "id": "AQEBAQEBAQEBAQEBAQEBAQ", "fingerprint": "A".repeat(43), "outcome": "tracked" },
        "client_meta": { "status": status },
        "birth_rev": null,
    });
    Ok(StoredValue::from_json_bytes(&serde_json::to_vec(&json)?)?)
}

fn refs(metas: &[MetaEntry]) -> Vec<String> {
    metas.iter().map(|entry| entry.phx_ref().to_string()).collect()
}

async fn publish_raw(
    client: &async_nats::Client,
    config: &PresenceConfig,
    kv_key: &KvKey,
    value: &StoredValue,
    ttl: &str,
) -> Result<(), BoxError> {
    let mut headers = HeaderMap::new();
    headers.insert(NATS_MESSAGE_TTL, ttl);
    headers.insert(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, "0");
    jetstream::new(client.clone())
        .publish_with_headers(
            config.bucket().subject_for(kv_key),
            headers,
            value.to_json_bytes()?.into(),
        )
        .await?
        .await?;
    Ok(())
}

#[tokio::test]
async fn two_holders_on_one_key_share_the_key() -> TestResult {
    let server = server_or_skip!();
    let presence = Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?;
    let (topic, key) = lobby()?;
    let first = presence.tracker()?;
    let second = presence.tracker()?;
    first.track(&topic, &key, meta("online")?).await?;
    second.track(&topic, &key, meta("away")?).await?;

    let watch = presence.watch(topic.clone()).await?;
    let state = watch.state();
    assert_eq!(state.len(), 1);
    assert_eq!(watch.get_by_key(&key).len(), 2);
    assert_eq!(watch.counters().rejected(), 0);
    Ok(())
}

#[tokio::test]
async fn untrack_emits_a_leave_with_one_meta() -> TestResult {
    let server = server_or_skip!();
    let presence = Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?;
    let (topic, key) = lobby()?;
    let staying = presence.tracker()?;
    let leaving = presence.tracker()?;
    staying.track(&topic, &key, meta("online")?).await?;
    let leaving_ref = applied(leaving.track(&topic, &key, meta("online")?).await?)
        .stored_ref()
        .clone();

    let watch = presence.watch(topic.clone()).await?;
    let (state, mut diffs) = watch.sync_state();
    assert_eq!(state.presences().get(&key).ok_or("not tracked")?.len(), 2);
    leaving.untrack(&topic, &key).await?;

    let diff = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    assert!(diff.joins().is_empty());
    assert_eq!(
        refs(diff.leaves().get(&key).ok_or("no leave")?),
        [leaving_ref.to_string()]
    );
    assert_eq!(watch.get_by_key(&key).len(), 1);
    Ok(())
}

#[tokio::test]
async fn update_emits_leave_and_join_with_previous_ref() -> TestResult {
    let server = server_or_skip!();
    let presence = Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?;
    let (topic, key) = lobby()?;
    let tracker = presence.tracker()?;
    tracker.track(&topic, &key, meta("online")?).await?;
    let watch = presence.watch(topic.clone()).await?;
    let (state, mut diffs) = watch.sync_state();
    let old_ref = state.presences().get(&key).ok_or("not tracked")?[0].phx_ref().clone();

    let new_ref = applied(tracker.update(&topic, &key, meta("away")?).await?)
        .stored_ref()
        .clone();
    let diff = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    let joined = diff.joins().get(&key).ok_or("no join")?;
    let left = diff.leaves().get(&key).ok_or("no leave")?;
    assert_eq!(refs(left), [old_ref.to_string()]);
    assert_eq!(refs(joined), [new_ref.to_string()]);
    assert_eq!(joined[0].phx_ref_prev(), Some(&old_ref));
    assert_eq!(joined[0].meta(), &meta("away")?);
    Ok(())
}

#[tokio::test]
async fn lease_expiry_emits_a_leave_from_the_marker() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::new(
        BucketName::default(),
        LeaseTtl::try_from(Duration::from_secs(1))?,
        HeartbeatInterval::try_from(Duration::from_millis(400))?,
        MarkerTtl::try_from(Duration::from_secs(2))?,
        ShardCount::DEFAULT,
    )?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let watch = presence.watch(topic.clone()).await?;
    let mut diffs = watch.subscribe();

    let kv_key = EntryKey::new(topic.clone(), key.clone(), HolderId::generate()?).encode(config.shards())?;
    let value = foreign_value(&topic, &key, "online")?;
    publish_raw(&client, &config, &kv_key, &value, &config.lease_ttl().header_value()).await?;

    let joined = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    assert_eq!(refs(joined.joins().get(&key).ok_or("no join")?), ["Fq1"]);
    let left = next_diff(&mut diffs, config.lease_ttl().get() + config.marker_ttl().get()).await?;
    assert_eq!(refs(left.leaves().get(&key).ok_or("no leave")?), ["Fq1"]);
    assert!(watch.state().is_empty());
    assert_eq!(watch.counters().swept(), 0);
    Ok(())
}

#[tokio::test]
async fn mismatched_entries_are_dropped_and_counted() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let watch = presence.watch(topic.clone()).await?;
    let mut diffs = watch.subscribe();
    let ttl = config.lease_ttl().header_value();

    let mallory: PresenceKey = "mallory".parse()?;
    let lying_key = EntryKey::new(topic.clone(), mallory.clone(), HolderId::generate()?).encode(config.shards())?;
    let claims_ana = foreign_value(&topic, &key, "online")?;
    publish_raw(&client, &config, &lying_key, &claims_ana, &ttl).await?;

    let holder = HolderId::generate()?;
    let wrong_shard = KvKey::try_from(format!("s01.{}.{holder}.{}", key.token(), topic.tokens()))?;
    publish_raw(&client, &config, &wrong_shard, &claims_ana, &ttl).await?;

    let tracker = presence.tracker()?;
    tracker.track(&topic, &key, meta("online")?).await?;
    let diff = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    assert_eq!(diff.joins().len(), 1);
    assert_eq!(diff.joins().get(&key).ok_or("no join")?.len(), 1);
    assert!(diff.joins().get(&mallory).is_none());
    assert_eq!(watch.counters().rejected(), 1);
    assert_eq!(watch.state().len(), 1);
    Ok(())
}

#[tokio::test]
async fn rapid_tracks_coalesce_into_one_diff() -> TestResult {
    let server = server_or_skip!();
    let presence = Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?;
    let topic: Topic = "room:lobby".parse()?;
    let watch = presence.watch(topic.clone()).await?;
    let mut diffs = watch.subscribe();

    let mut tracks = JoinSet::new();
    for index in 0..10 {
        let tracker = presence.tracker()?;
        let topic = topic.clone();
        let key: PresenceKey = format!("user-{index}").parse()?;
        tracks.spawn(async move {
            tracker
                .track(&topic, &key, meta("online")?)
                .await
                .map_err(BoxError::from)
        });
    }
    while let Some(result) = tracks.join_next().await {
        result??;
    }

    let diff = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    assert_eq!(diff.joins().len(), 10);
    assert!(diff.leaves().is_empty());
    assert_quiet(&mut diffs).await;
    assert_eq!(watch.state().len(), 10);
    Ok(())
}

fn barrier(
    generation: StreamGeneration,
    topic: &Topic,
    key: &PresenceKey,
    holder: &HolderId,
    target: EntryRevision,
    within: Duration,
) -> ReadBarrier {
    let entry = EntryKey::new(topic.clone(), key.clone(), *holder);
    ReadBarrier::new(generation, entry, target, UnixMillis::now().saturating_add(within))
}

async fn read(
    presence: &Presence,
    watch: &TopicWatch,
    key: &PresenceKey,
    holder: &HolderId,
    target: EntryRevision,
) -> Result<ViewSnapshot, BarrierError> {
    let barrier = barrier(presence.generation(), watch.topic(), key, holder, target, DIFF_TIMEOUT);
    watch.read(&barrier).await
}

async fn provision(server: &NatsServer) -> Result<Presence, BoxError> {
    Ok(Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?)
}

#[tokio::test]
async fn barrier_is_satisfied_by_the_entry_revision() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let watch = presence.watch(topic.clone()).await?;
    let tracker = presence.tracker()?;
    let tracked = applied(tracker.track(&topic, &key, meta("online")?).await?);

    let snapshot = read(&presence, &watch, &key, tracker.holder(), tracked.revision()).await?;
    assert_eq!(
        refs(snapshot.presences().get(&key).ok_or("not in view")?),
        [tracked.stored_ref().to_string()]
    );
    assert_eq!(snapshot.cursor(), watch.cursor());
    Ok(())
}

#[tokio::test]
async fn barrier_is_satisfied_by_a_superseding_retirement() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let tracker = presence.tracker()?;
    let tracked = applied(tracker.track(&topic, &key, meta("online")?).await?);
    tracker.untrack(&topic, &key).await?;
    let watch = presence.watch(topic.clone()).await?;

    let snapshot = read(&presence, &watch, &key, tracker.holder(), tracked.revision()).await?;
    assert!(snapshot.presences().get(&key).is_none());
    Ok(())
}

#[tokio::test]
async fn unrelated_stream_advance_does_not_satisfy_the_barrier() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let ana = presence.tracker()?;
    applied(ana.track(&topic, &key, meta("online")?).await?);
    let bob = presence.tracker()?;
    let later = applied(bob.track(&topic, &"bob".parse()?, meta("online")?).await?).revision();
    let watch = presence.watch(topic.clone()).await?;

    let result = read(&presence, &watch, &key, ana.holder(), later).await;
    assert_eq!(result, Err(BarrierError::Unavailable { target: later }));
    Ok(())
}

#[tokio::test]
async fn barrier_past_the_window_expires_without_clamping() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let tracker = presence.tracker()?;
    let revision = applied(tracker.track(&topic, &key, meta("online")?).await?).revision();
    let watch = presence.watch(topic.clone()).await?;
    let future = EntryRevision::from(revision.get() + 1_000);

    let started = tokio::time::Instant::now();
    let result = read(&presence, &watch, &key, tracker.holder(), future).await;
    assert_eq!(result, Err(BarrierError::Expired { target: future }));
    assert!(started.elapsed() <= Duration::from_millis(1_500));
    Ok(())
}

#[tokio::test]
async fn barrier_from_another_generation_is_refused() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let tracker = presence.tracker()?;
    let revision = applied(tracker.track(&topic, &key, meta("online")?).await?).revision();
    let watch = presence.watch(topic.clone()).await?;
    let stale = StreamGeneration::generate()?;

    let result = watch
        .read(&barrier(stale, &topic, &key, tracker.holder(), revision, DIFF_TIMEOUT))
        .await;
    assert_eq!(
        result,
        Err(BarrierError::GenerationChanged {
            expected: stale,
            current: presence.generation(),
        })
    );
    Ok(())
}

#[tokio::test]
async fn local_cursor_chains_every_published_diff() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let watch = presence.watch(topic.clone()).await?;
    let (snapshot, mut diffs) = watch.sync_state();
    let tracker = presence.tracker()?;

    tracker.track(&topic, &key, meta("online")?).await?;
    let first = next_view_diff(&mut diffs, DIFF_TIMEOUT).await?;
    assert_eq!(
        snapshot.cursor().follow(&first.cursor(), first.prev()),
        CursorStep::Next
    );
    assert_eq!(first.cursor().seq().get(), 2);

    tracker.update(&topic, &key, meta("away")?).await?;
    let second = next_view_diff(&mut diffs, DIFF_TIMEOUT).await?;
    assert_eq!(first.cursor().follow(&second.cursor(), second.prev()), CursorStep::Next);
    assert_eq!(
        snapshot.cursor().follow(&second.cursor(), second.prev()),
        CursorStep::Gap
    );
    Ok(())
}

#[derive(Clone)]
struct Enrich;

impl MetaFetcher for Enrich {
    async fn fetch(&self, _topic: &Topic, entries: Vec<FetchEntry>) -> Result<Vec<FetchEntry>, FetchError> {
        entries
            .into_iter()
            .map(|entry| {
                let mut rendered = entry.meta().as_map().clone();
                rendered.insert("region".to_owned(), serde_json::json!("eu"));
                let rendered = Meta::try_from(rendered).map_err(FetchError::failed)?;
                Ok(entry.with_meta(rendered))
            })
            .collect()
    }
}

#[tokio::test]
async fn rendered_view_uses_client_meta_enrichment_and_birth_order() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let elder = presence.tracker()?;
    let younger = presence.tracker()?;
    let elder_ref = applied(elder.track(&topic, &key, meta("online")?).await?)
        .stored_ref()
        .clone();
    applied(younger.track(&topic, &key, meta("online")?).await?);
    let watch = presence
        .watch_with(topic.clone(), CoalesceWindow::default(), Enrich)
        .await?;
    let mut diffs = watch.subscribe();

    let updated = applied(elder.update(&topic, &key, meta("away")?).await?);
    let diff = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    let joined = &diff.joins().get(&key).ok_or("no join")?[0];
    let expected: Meta = serde_json::from_value(serde_json::json!({ "status": "away", "region": "eu" }))?;
    assert_eq!(joined.meta(), &expected);
    assert_eq!(joined.phx_ref(), &ViewRef::derive(updated.stored_ref(), &expected));
    assert_eq!(
        joined.phx_ref_prev(),
        Some(&ViewRef::derive(
            &elder_ref,
            &serde_json::from_value(serde_json::json!({ "status": "online", "region": "eu" }))?
        ))
    );

    let metas = watch.get_by_key(&key);
    assert_eq!(metas.len(), 2);
    assert_eq!(metas[0].phx_ref(), joined.phx_ref());
    Ok(())
}

#[tokio::test]
async fn view_ref_is_stable_on_a_noop_update_and_changes_with_meta() -> TestResult {
    let server = server_or_skip!();
    let presence = provision(&server).await?;
    let (topic, key) = lobby()?;
    let tracker = presence.tracker()?;
    tracker.track(&topic, &key, meta("online")?).await?;
    let watch = presence.watch(topic.clone()).await?;
    let mut diffs = watch.subscribe();
    let before = watch.get_by_key(&key)[0].phx_ref().clone();

    let noop = applied(tracker.update(&topic, &key, meta("online")?).await?);
    read(&presence, &watch, &key, tracker.holder(), noop.revision()).await?;
    assert_eq!(watch.get_by_key(&key)[0].phx_ref(), &before);
    assert_quiet(&mut diffs).await;

    tracker.update(&topic, &key, meta("away")?).await?;
    let diff = next_diff(&mut diffs, DIFF_TIMEOUT).await?;
    let after = diff.joins().get(&key).ok_or("no join")?[0].phx_ref().clone();
    assert_ne!(after, before);
    Ok(())
}
