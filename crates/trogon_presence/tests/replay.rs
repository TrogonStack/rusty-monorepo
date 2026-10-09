mod common;

use std::time::{Duration, SystemTime};

use async_nats::header::NATS_MESSAGE_TTL;
use async_nats::jetstream::{self, stream::Stream};
use async_nats::HeaderMap;
use common::NatsServer;
use futures_util::StreamExt;
use tokio::sync::broadcast;
use trogon_presence::value::StoredValue;
use trogon_presence::watch::engine::{
    Classified, Classifier, LiveSet, ShardClassifier, Slot, StalePolicy, SweepOutcome, WatchCore,
};
use trogon_presence::watch::replay::{RebuildBudget, ReconcileInterval, ReplayConsumer, ReplayFilters, Watermark};
use trogon_presence::{
    BucketName, Diff, EntryKey, EntryRevision, HeartbeatInterval, HolderId, KvKey, LeaseTtl, MarkerTtl, Meta,
    NoopFetcher, Presence, PresenceConfig, PresenceKey, Presences, ProvisionOptions, ReadBarrier, Readiness,
    ShardCount, Topic, TopicWatch, UnixMillis, ViewDiff, ViewShard, WatchError, WatchOptions, WriteOutcome,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
type BoxError = Box<dyn std::error::Error + Send + Sync>;

const CONVERGE_TIMEOUT: Duration = Duration::from_secs(10);
const LONG_TTL: &str = "300s";

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn lobby() -> Result<(Topic, PresenceKey), BoxError> {
    Ok(("room:lobby".parse()?, "ana@x.io".parse()?))
}

fn meta(status: &str) -> Result<Meta, serde_json::Error> {
    serde_json::from_value(serde_json::json!({ "status": status }))
}

fn fast_config() -> Result<PresenceConfig, BoxError> {
    Ok(PresenceConfig::new(
        BucketName::default(),
        LeaseTtl::try_from(Duration::from_secs(1))?,
        HeartbeatInterval::try_from(Duration::from_millis(400))?,
        MarkerTtl::try_from(Duration::from_secs(3))?,
        ShardCount::DEFAULT,
    )?)
}

fn stored(topic: &Topic, key: &PresenceKey, phx_ref: &str) -> Result<StoredValue, BoxError> {
    let json = serde_json::json!({
        "v": 2,
        "topic": topic,
        "key": key,
        "phx_ref": phx_ref,
        "phx_ref_prev": null,
        "meta": { "status": "online" },
        "lifetime": "AAAAAAAAAAAAAAAAAAAAAA",
        "mutation_seq": "1",
        "last_op": { "id": "AQEBAQEBAQEBAQEBAQEBAQ", "fingerprint": "A".repeat(43), "outcome": "tracked" },
        "client_meta": { "status": "online" },
        "birth_rev": null,
    });
    Ok(StoredValue::from_json_bytes(&serde_json::to_vec(&json)?)?)
}

fn entry_key(config: &PresenceConfig, topic: &Topic, key: &PresenceKey) -> Result<KvKey, BoxError> {
    Ok(EntryKey::new(topic.clone(), key.clone(), HolderId::generate()?).encode(config.shards())?)
}

async fn publish(
    client: &async_nats::Client,
    config: &PresenceConfig,
    kv_key: &KvKey,
    payload: Vec<u8>,
    ttl: &str,
) -> Result<(), BoxError> {
    let mut headers = HeaderMap::new();
    headers.insert(NATS_MESSAGE_TTL, ttl);
    jetstream::new(client.clone())
        .publish_with_headers(config.bucket().subject_for(kv_key), headers, payload.into())
        .await?
        .await?;
    Ok(())
}

async fn flood(
    client: &async_nats::Client,
    config: &PresenceConfig,
    topic: &Topic,
    entries: usize,
) -> Result<(), BoxError> {
    let context = jetstream::new(client.clone());
    let mut acks = Vec::new();
    for index in 0..entries {
        let key: PresenceKey = format!("user-{index}").parse()?;
        let kv_key = entry_key(config, topic, &key)?;
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_TTL, LONG_TTL);
        let payload = stored(topic, &key, "Fq1")?.to_json_bytes()?;
        acks.push(
            context
                .publish_with_headers(config.bucket().subject_for(&kv_key), headers, payload.into())
                .await?,
        );
        if acks.len() == 1_000 {
            for ack in acks.drain(..) {
                ack.await?;
            }
        }
    }
    for ack in acks {
        ack.await?;
    }
    Ok(())
}

async fn stream_of(client: &async_nats::Client, config: &PresenceConfig) -> Result<Stream, BoxError> {
    Ok(jetstream::new(client.clone())
        .get_stream(config.bucket().stream_name())
        .await?)
}

async fn consumer_names(stream: &Stream) -> Result<Vec<String>, BoxError> {
    let mut names = Vec::new();
    let mut listing = stream.consumer_names();
    while let Some(name) = listing.next().await {
        names.push(name?);
    }
    Ok(names)
}

async fn read_after(
    presence: &Presence,
    watch: &TopicWatch,
    key: &PresenceKey,
    holder: &HolderId,
    target: EntryRevision,
) -> Result<Presences, BoxError> {
    let entry = EntryKey::new(watch.topic().clone(), key.clone(), *holder);
    let expires = UnixMillis::now().saturating_add(CONVERGE_TIMEOUT);
    let barrier = ReadBarrier::new(presence.generation(), entry, target, expires);
    Ok(watch.read(&barrier).await?.into_presences())
}

async fn diff_matching(
    diffs: &mut broadcast::Receiver<ViewDiff>,
    accept: impl Fn(&Diff) -> bool,
) -> Result<Diff, BoxError> {
    let deadline = tokio::time::Instant::now() + CONVERGE_TIMEOUT;
    loop {
        let diff = tokio::time::timeout_at(deadline, diffs.recv()).await??.into_diff();
        if accept(&diff) {
            return Ok(diff);
        }
    }
}

async fn eventually(mut check: impl FnMut() -> bool) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + CONVERGE_TIMEOUT;
    while !check() {
        if tokio::time::Instant::now() > deadline {
            return Err("condition did not hold in time".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[tokio::test]
async fn empty_bucket_is_ready_immediately() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, _) = lobby()?;

    let watch = presence.watch(topic.clone()).await?;
    assert_eq!(watch.readiness(), Readiness::Ready);
    assert!(watch.state().is_empty());

    let stream = stream_of(&client, &config).await?;
    let filters = Classifier::new(config.bucket(), config.shards(), topic).filters();
    let mut applied = 0;
    let replay = ReplayConsumer::rebuild(&stream, &filters, RebuildBudget::default(), |_| applied += 1).await?;
    assert_eq!(applied, 0);
    assert!(replay.watermark().await.is_caught_up());
    Ok(())
}

#[tokio::test]
async fn busy_replay_waits_for_pending_to_reach_zero() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, _) = lobby()?;
    let seeded = 500;
    flood(&client, &config, &topic, seeded).await?;

    let writer = {
        let (client, config, topic) = (client.clone(), config.clone(), topic.clone());
        tokio::spawn(async move {
            let key: PresenceKey = "busy".parse()?;
            let kv_key = entry_key(&config, &topic, &key)?;
            for _ in 0..200 {
                publish(
                    &client,
                    &config,
                    &kv_key,
                    stored(&topic, &key, "Fq2")?.to_json_bytes()?,
                    LONG_TTL,
                )
                .await?;
            }
            Ok::<(), BoxError>(())
        })
    };

    let stream = stream_of(&client, &config).await?;
    let filters = Classifier::new(config.bucket(), config.shards(), topic).filters();
    let mut subjects = std::collections::HashSet::new();
    let mut last_pending = None;
    let replay = ReplayConsumer::rebuild(&stream, &filters, RebuildBudget::default(), |record| {
        subjects.insert(record.subject().to_owned());
        last_pending = Some(record.pending());
    })
    .await?;
    assert!(last_pending.is_some_and(|pending| pending.is_zero()));
    assert!(subjects.len() >= seeded);
    writer.await??;
    drop(replay);
    Ok(())
}

#[tokio::test]
async fn overwrite_during_replay_converges_to_the_last_value() -> TestResult {
    let server = server_or_skip!();
    let presence = Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?;
    let (topic, key) = lobby()?;
    let tracker = presence.tracker()?;
    tracker.track(&topic, &key, meta("v0")?).await?;

    let writer = {
        let (topic, key) = (topic.clone(), key.clone());
        tokio::spawn(async move {
            let mut last = None;
            for round in 1..=40 {
                if let WriteOutcome::Applied(receipt) =
                    tracker.update(&topic, &key, meta(&format!("v{round}"))?).await?
                {
                    last = Some((receipt.stored_ref().clone(), receipt.revision()));
                }
            }
            Ok::<_, BoxError>((last.ok_or("no update applied")?, tracker))
        })
    };
    let watch = presence.watch(topic.clone()).await?;
    let ((last_ref, revision), tracker) = writer.await??;

    let state = read_after(&presence, &watch, &key, tracker.holder(), revision).await?;
    let metas = state.get(&key).ok_or("not tracked")?;
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].phx_ref().to_string(), last_ref.to_string());
    assert_eq!(metas[0].meta(), &meta("v40")?);
    Ok(())
}

#[tokio::test]
async fn expiry_marker_during_replay_produces_a_leave() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let holder = HolderId::generate()?;
    let kv_key = EntryKey::new(topic.clone(), key.clone(), holder).encode(config.shards())?;
    publish(
        &client,
        &config,
        &kv_key,
        stored(&topic, &key, "Fq1")?.to_json_bytes()?,
        &config.lease_ttl().header_value(),
    )
    .await?;

    let watch = presence.watch(topic.clone()).await?;
    let mut diffs = watch.subscribe();
    assert_eq!(watch.state().len(), 1);
    tokio::time::sleep(config.lease_ttl().get() + Duration::from_millis(500)).await;

    let stream = stream_of(&client, &config).await?;
    let classifier = Classifier::new(config.bucket(), config.shards(), topic.clone());
    let mut live = LiveSet::default();
    ReplayConsumer::rebuild(&stream, &classifier.filters(), RebuildBudget::default(), |record| {
        if let Classified::Observed(observation) = classifier.classify(&record) {
            live.apply(observation);
        }
    })
    .await?;
    assert!(live.is_empty());
    assert!(live.retirement_revision(&Slot::new(key.clone(), holder)).is_some());

    let left = diff_matching(&mut diffs, |diff| diff.leaves().get(&key).is_some()).await?;
    assert!(left.joins().is_empty());
    assert!(watch.state().is_empty());
    assert_eq!(watch.counters().swept(), 0);
    Ok(())
}

#[tokio::test]
async fn deleting_the_consumer_rebuilds_and_reconverges() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let first = presence.tracker()?;
    first.track(&topic, &key, meta("online")?).await?;
    let watch = presence.watch(topic.clone()).await?;
    assert_eq!(watch.get_by_key(&key).len(), 1);

    let stream = stream_of(&client, &config).await?;
    let names = consumer_names(&stream).await?;
    assert!(!names.is_empty());
    for name in &names {
        stream.delete_consumer(name).await?;
    }
    eventually(|| watch.counters().reconnects() >= 1).await?;

    let second = presence.tracker()?;
    let WriteOutcome::Applied(receipt) = second.track(&topic, &key, meta("away")?).await? else {
        return Err("second track did not apply".into());
    };
    let state = read_after(&presence, &watch, &key, second.holder(), receipt.revision()).await?;
    assert_eq!(state.get(&key).ok_or("not tracked")?.len(), 2);
    assert_eq!(watch.readiness(), Readiness::Ready);
    let fresh = consumer_names(&stream).await?;
    assert!(fresh.iter().all(|name| !names.contains(name)));
    Ok(())
}

#[tokio::test]
async fn lagging_consumer_suspends_the_sweep() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let kv_key = entry_key(&config, &topic, &key)?;
    publish(
        &client,
        &config,
        &kv_key,
        stored(&topic, &key, "Fq1")?.to_json_bytes()?,
        LONG_TTL,
    )
    .await?;

    let stream = stream_of(&client, &config).await?;
    let classifier = ShardClassifier::new(config.bucket(), config.shards(), ViewShard::of(&topic, config.shards()));
    let mut live = LiveSet::default();
    let mut replay = ReplayConsumer::rebuild(&stream, &classifier.filters(), RebuildBudget::default(), |record| {
        if let Classified::Observed((_, observation)) = classifier.classify(&record) {
            live.apply(observation);
        }
    })
    .await?;
    let mut core = WatchCore::new(live);
    assert_eq!(core.flush().joins().len(), 1);

    publish(
        &client,
        &config,
        &kv_key,
        stored(&topic, &key, "Fq2")?.to_json_bytes()?,
        LONG_TTL,
    )
    .await?;
    let watermark = replay.watermark().await;
    assert!(
        matches!(watermark, Watermark::Lagging { .. } | Watermark::Behind { .. }),
        "{watermark:?}"
    );

    let policy = StalePolicy::from(&config);
    let far_future = SystemTime::now() + Duration::from_secs(3_600);
    assert!(core.has_stale(far_future, policy));
    assert_eq!(
        core.sweep(watermark, far_future, policy),
        SweepOutcome::Suspended(watermark)
    );
    assert!(core.flush().is_empty());

    let record = tokio::time::timeout(CONVERGE_TIMEOUT, replay.next()).await??;
    if let Classified::Observed((_, observation)) = classifier.classify(&record) {
        core.apply(observation);
    }
    let caught_up = replay.watermark().await;
    assert!(caught_up.is_caught_up(), "{caught_up:?}");
    Ok(())
}

async fn busy_bystanders(stream: &Stream, config: &PresenceConfig, consumers: usize) -> Result<(), BoxError> {
    for index in 0..consumers {
        stream
            .create_consumer(jetstream::consumer::pull::Config {
                name: Some(format!("bystander_{index}")),
                filter_subject: config.bucket().subjects_filter(),
                memory_storage: true,
                ..Default::default()
            })
            .await?;
    }
    Ok(())
}

async fn flood_controls(client: async_nats::Client, config: PresenceConfig) -> Result<(), BoxError> {
    let context = jetstream::new(client);
    let subjects = (0..16)
        .map(|_| {
            Ok(config
                .bucket()
                .subject_for(&KvKey::try_from(format!("ctl.direct.{}", HolderId::generate()?))?))
        })
        .collect::<Result<Vec<_>, BoxError>>()?;
    loop {
        let mut acks = Vec::with_capacity(512);
        for subject in subjects.iter().cycle().take(512) {
            acks.push(context.publish(subject.clone(), "{}".into()).await?);
        }
        for ack in acks {
            ack.await?;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn watermark_never_certifies_an_acknowledged_publish_before_it_is_applied() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let kv_key = entry_key(&config, &topic, &key)?;
    let stream = stream_of(&client, &config).await?;
    busy_bystanders(&stream, &config, 64).await?;
    let classifier = ShardClassifier::new(config.bucket(), config.shards(), ViewShard::of(&topic, config.shards()));
    let mut replay = ReplayConsumer::rebuild(&stream, &classifier.filters(), RebuildBudget::default(), |_| {}).await?;
    let flooding = tokio::spawn(flood_controls(server.client().await, config.clone()));

    for round in 0..200 {
        publish(
            &client,
            &config,
            &kv_key,
            stored(&topic, &key, &format!("Fq{round}"))?.to_json_bytes()?,
            LONG_TTL,
        )
        .await?;
        let watermark = replay.watermark().await;
        assert!(!watermark.is_caught_up(), "round {round}: {watermark:?}");
        let record = tokio::time::timeout(CONVERGE_TIMEOUT, replay.next()).await??;
        assert_eq!(record.subject(), config.bucket().subject_for(&kv_key));
    }
    flooding.abort();
    Ok(())
}

#[tokio::test]
async fn reconcile_repairs_an_injected_divergence() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let kv_key = entry_key(&config, &topic, &key)?;
    publish(
        &client,
        &config,
        &kv_key,
        stored(&topic, &key, "Fq1")?.to_json_bytes()?,
        LONG_TTL,
    )
    .await?;

    let options = WatchOptions::default().with_reconcile(ReconcileInterval::try_from(Duration::from_secs(1))?);
    let watch = presence.watch_with_options(topic.clone(), options, NoopFetcher).await?;
    let mut diffs = watch.subscribe();
    assert_eq!(watch.get_by_key(&key).len(), 1);

    let stream = stream_of(&client, &config).await?;
    stream.purge().filter(config.bucket().subject_for(&kv_key)).await?;

    let left = diff_matching(&mut diffs, |diff| diff.leaves().get(&key).is_some()).await?;
    assert!(left.joins().is_empty());
    assert!(watch.state().is_empty());
    assert!(watch.counters().reconciles() >= 1);
    assert_eq!(watch.counters().swept(), 0);
    Ok(())
}

#[tokio::test]
async fn control_records_never_appear_in_the_view() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key) = lobby()?;
    let watch = presence.watch(topic.clone()).await?;
    let holder = HolderId::generate()?;
    let controls = [
        format!("ctl.writer.{}", key.token()),
        format!("ctl.direct.{holder}"),
        format!("ctl.receipt.direct.{holder}.AAAAAAAAAAAAAAAAAAAAAA"),
    ];
    let payload = stored(&topic, &key, "Fq1")?.to_json_bytes()?;
    for control in &controls {
        publish(
            &client,
            &config,
            &KvKey::try_from(control.clone())?,
            payload.clone(),
            LONG_TTL,
        )
        .await?;
    }
    let tracker = presence.tracker()?;
    let WriteOutcome::Applied(receipt) = tracker.track(&topic, &key, meta("online")?).await? else {
        return Err("track did not apply".into());
    };
    let state = read_after(&presence, &watch, &key, tracker.holder(), receipt.revision()).await?;
    assert_eq!(state.len(), 1);
    assert_eq!(state.get(&key).ok_or("not tracked")?.len(), 1);
    assert_eq!(watch.counters().rejected(), 0);

    let stream = stream_of(&client, &config).await?;
    let classifier = ShardClassifier::new(config.bucket(), config.shards(), ViewShard::of(&topic, config.shards()));
    let mut routed = 0;
    let mut seen = Vec::new();
    ReplayConsumer::rebuild(&stream, &classifier.filters(), RebuildBudget::default(), |record| {
        seen.push(record.subject().to_owned());
        if matches!(classifier.classify(&record), Classified::Observed(_)) {
            routed += 1;
        }
    })
    .await?;
    assert_eq!(routed, 1);
    assert!(seen.iter().all(|subject| !subject.contains(".ctl.")));

    let everything = ReplayFilters::from(config.bucket().subjects_filter());
    let mut controls_seen = 0;
    ReplayConsumer::rebuild(&stream, &everything, RebuildBudget::default(), |record| {
        if record.subject().contains(".ctl.") {
            controls_seen += 1;
            assert!(matches!(
                classifier.classify(&record),
                Classified::Rejected { evict: None, .. }
            ));
        }
    })
    .await?;
    assert!(controls_seen >= controls.len());
    Ok(())
}

#[tokio::test]
async fn rebuild_budget_returns_not_ready_under_a_flood() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, _) = lobby()?;
    flood(&client, &config, &topic, 20_000).await?;
    assert_eq!(RebuildBudget::default().get(), Duration::from_secs(5));

    let budget = RebuildBudget::try_from(Duration::from_millis(50))?;
    let stream = stream_of(&client, &config).await?;
    let filters = Classifier::new(config.bucket(), config.shards(), topic.clone()).filters();
    let result = ReplayConsumer::rebuild(&stream, &filters, budget, |_| {}).await;
    let Err(err) = result else {
        return Err("replay certified a flood within 50ms".into());
    };
    assert!(err.is_not_ready(), "{err}");

    let options = WatchOptions::default().with_budget(budget);
    let result = presence.watch_with_options(topic, options, NoopFetcher).await;
    assert!(matches!(result, Err(WatchError::NotReady(_))), "{:?}", result.err());

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(consumer_names(&stream).await?.is_empty());
    Ok(())
}
