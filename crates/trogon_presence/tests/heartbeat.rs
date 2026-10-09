mod common;

use std::time::{Duration, Instant};

use async_nats::header::{NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, NATS_MARKER_REASON, NATS_MESSAGE_TTL};
use async_nats::jetstream::stream::LastRawMessageErrorKind;
use async_nats::jetstream::{self, message::StreamMessage};
use async_nats::HeaderMap;
use common::NatsServer;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use trogon_presence::value::StoredValue;
use trogon_presence::{
    BucketName, EntryKey, HeartbeatInterval, HolderId, LeaseTtl, LifetimeId, MarkerTtl, Meta, Presence, PresenceConfig,
    PresenceEvent, PresenceKey, ProvisionOptions, ShardCount, StoredRef, Topic, WriteOutcome, WriteReceipt,
};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

const DEFAULT_MARKER_TTL: Duration = Duration::from_secs(5);

/// A marker ttl wide enough that a host starved for several seconds between the purge write
/// and the follow-up read still observes the tombstone before the server reclaims it.
const RESURRECTION_CHECK_MARKER_TTL: Duration = Duration::from_secs(20);

fn config(lease: Duration, interval: Duration, marker: Duration) -> Result<PresenceConfig, TestError> {
    Ok(PresenceConfig::new(
        BucketName::default(),
        LeaseTtl::try_from(lease)?,
        HeartbeatInterval::try_from(interval)?,
        MarkerTtl::try_from(marker)?,
        ShardCount::DEFAULT,
    )?)
}

fn fast_config() -> Result<PresenceConfig, TestError> {
    config(Duration::from_secs(1), Duration::from_millis(400), DEFAULT_MARKER_TTL)
}

fn resilient_untrack_config() -> Result<PresenceConfig, TestError> {
    config(
        Duration::from_secs(1),
        Duration::from_millis(400),
        RESURRECTION_CHECK_MARKER_TTL,
    )
}

async fn raw_entry(
    client: &async_nats::Client,
    config: &PresenceConfig,
    entry: &EntryKey,
) -> Result<StreamMessage, TestError> {
    let stream = jetstream::new(client.clone())
        .get_stream(config.bucket().stream_name())
        .await?;
    let subject = config.bucket().subject_for(&entry.encode(config.shards())?);
    Ok(stream.get_last_raw_message_by_subject(&subject).await?)
}

enum LastRecord {
    Present(StreamMessage),
    Reclaimed,
}

/// Like [`raw_entry`], but a subject with no message left at all is reported as
/// [`LastRecord::Reclaimed`] instead of an error: the last message for a subject is a tombstone
/// published with its own ttl (see `MarkerTtl`), and the server is free to reclaim it once that
/// ttl elapses. A reclaimed subject is the strongest possible proof that nothing resurrected the
/// entry, since a resurrection would itself be a live message the server has not reclaimed.
async fn last_record_or_reclaimed(
    client: &async_nats::Client,
    config: &PresenceConfig,
    entry: &EntryKey,
) -> Result<LastRecord, TestError> {
    let stream = jetstream::new(client.clone())
        .get_stream(config.bucket().stream_name())
        .await?;
    let subject = config.bucket().subject_for(&entry.encode(config.shards())?);
    match stream.get_last_raw_message_by_subject(&subject).await {
        Ok(message) => Ok(LastRecord::Present(message)),
        Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(LastRecord::Reclaimed),
        Err(err) => Err(err.into()),
    }
}

fn header<'a>(message: &'a StreamMessage, name: &str) -> Option<&'a str> {
    message.headers.get(name).map(|value| value.as_str())
}

fn applied(outcome: WriteOutcome) -> WriteReceipt {
    match outcome {
        WriteOutcome::Applied(receipt) => receipt,
        other => panic!("expected an applied write, got {other:?}"),
    }
}

fn foreign_value(topic: &str, key: &str, status: &str) -> Result<StoredValue, TestError> {
    let json = format!(
        r#"{{"v":2,"topic":{topic:?},"key":{key:?},"phx_ref":"Fq1","phx_ref_prev":null,"meta":{{"status":{status:?}}},"lifetime":"AAAAAAAAAAAAAAAAAAAAAA","mutation_seq":"1","last_op":{{"id":"AQEBAQEBAQEBAQEBAQEBAQ","fingerprint":"{}","outcome":"tracked"}},"client_meta":{{"status":{status:?}}},"birth_rev":null}}"#,
        "A".repeat(43)
    );
    Ok(StoredValue::from_json_bytes(json.as_bytes())?)
}

fn identity() -> Result<(Topic, PresenceKey, HolderId), TestError> {
    Ok(("room:lobby".parse()?, "ana@x.io".parse()?, HolderId::generate()?))
}

async fn wait_for_marker(
    client: &async_nats::Client,
    config: &PresenceConfig,
    entry: &EntryKey,
) -> Result<StreamMessage, TestError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let raw = raw_entry(client, config, entry).await?;
        if header(&raw, NATS_MARKER_REASON.as_ref()).is_some() {
            return Ok(raw);
        }
        assert!(Instant::now() < deadline, "entry never expired");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn heartbeat_keeps_an_entry_alive_past_its_lease() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;
    let tracked = applied(tracker.track(&topic, &key, Meta::default()).await?);
    let (phx_ref, revision) = (tracked.stored_ref().clone(), tracked.revision());

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last_seq = revision.get();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let raw = raw_entry(&client, &config, &entry).await?;
        assert_eq!(header(&raw, NATS_MARKER_REASON.as_ref()), None, "entry expired");
        assert_eq!(StoredValue::from_json_bytes(&raw.payload)?.phx_ref(), &phx_ref);
        assert_eq!(header(&raw, NATS_MESSAGE_TTL.as_ref()), Some("1s"));
        last_seq = raw.sequence;
    }
    assert!(last_seq > revision.get(), "no heartbeat landed");
    let entries = tracker.entries().await?;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].revision().get() >= last_seq.saturating_sub(1));
    Ok(())
}

#[tokio::test]
async fn closing_the_presence_stops_heartbeats_and_the_entry_expires() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;
    tracker.track(&topic, &key, Meta::default()).await?;

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        header(&raw_entry(&client, &config, &entry).await?, NATS_MARKER_REASON.as_ref()),
        None
    );
    presence.close();
    let marker = wait_for_marker(&client, &config, &entry).await?;
    assert_eq!(header(&marker, NATS_MARKER_REASON.as_ref()), Some("MaxAge"));
    assert_eq!(tracker.entries().await?.len(), 1, "close must not untrack");
    Ok(())
}

#[tokio::test]
async fn dropping_the_presence_stops_heartbeats() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;
    tracker.track(&topic, &key, Meta::default()).await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;

    drop(presence);
    let marker = wait_for_marker(&client, &config, &entry).await?;
    assert_eq!(header(&marker, NATS_MARKER_REASON.as_ref()), Some("MaxAge"));
    Ok(())
}

#[tokio::test]
async fn close_and_untrack_purges_every_tracker() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let key: PresenceKey = "ana".parse()?;
    let mut entries = Vec::new();
    for topic in ["room:a", "room:b"] {
        let topic: Topic = topic.parse()?;
        let tracker = presence.tracker()?;
        tracker.track(&topic, &key, Meta::default()).await?;
        entries.push((tracker.clone(), EntryKey::new(topic, key.clone(), *tracker.holder())));
    }

    presence.close_and_untrack().await?;
    for (tracker, entry) in entries {
        assert!(tracker.entries().await?.is_empty());
        let raw = raw_entry(&client, &config, &entry).await?;
        assert_eq!(header(&raw, "KV-Operation"), Some("PURGE"));
    }
    Ok(())
}

struct Retrack {
    old_lifetime: LifetimeId,
    old_ref: StoredRef,
    new_lifetime: LifetimeId,
    new_ref: StoredRef,
}

async fn next_retrack(
    events: &mut tokio::sync::broadcast::Receiver<PresenceEvent>,
    within: Duration,
) -> Result<Retrack, TestError> {
    let event = tokio::time::timeout(within, async {
        loop {
            match events.recv().await {
                Ok(event @ PresenceEvent::Retracked { .. }) => return Ok(event),
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(err @ RecvError::Closed) => return Err(err),
            }
        }
    })
    .await??;
    let PresenceEvent::Retracked {
        old_lifetime,
        old_ref,
        new_lifetime,
        new_ref,
        ..
    } = event
    else {
        panic!("expected a retrack");
    };
    Ok(Retrack {
        old_lifetime,
        old_ref,
        new_lifetime,
        new_ref,
    })
}

#[tokio::test]
async fn foreign_overwrite_is_retracked_with_a_fresh_lifetime_and_ref() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let mut events = presence.events();
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let kv_key = entry.encode(config.shards())?;
    let tracker = presence.tracker_for(holder)?;
    let ours = applied(tracker.track(&topic, &key, Meta::default()).await?);

    let foreign = foreign_value("room:lobby", "ana@x.io", "away")?;
    let current = raw_entry(&client, &config, &entry).await?;
    let mut headers = HeaderMap::new();
    headers.insert(NATS_MESSAGE_TTL, config.lease_ttl().header_value());
    headers.insert(
        NATS_EXPECTED_LAST_SUBJECT_SEQUENCE,
        current.sequence.to_string().as_str(),
    );
    jetstream::new(client.clone())
        .publish_with_headers(
            config.bucket().subject_for(&kv_key),
            headers,
            foreign.to_json_bytes()?.into(),
        )
        .await?
        .await?;

    let retrack = next_retrack(&mut events, Duration::from_secs(3)).await?;
    assert_eq!(retrack.old_lifetime, ours.lifetime());
    assert_eq!(&retrack.old_ref, ours.stored_ref());
    assert_ne!(retrack.new_lifetime, ours.lifetime());
    assert_ne!(retrack.new_lifetime, foreign.lifetime());
    assert_ne!(&retrack.new_ref, ours.stored_ref());
    assert_ne!(&retrack.new_ref, foreign.phx_ref());

    let stored = StoredValue::from_json_bytes(&raw_entry(&client, &config, &entry).await?.payload)?;
    assert_eq!(stored.lifetime(), retrack.new_lifetime);
    assert_eq!(stored.phx_ref(), &retrack.new_ref);
    assert_eq!(stored.phx_ref_prev(), None);
    assert_eq!(stored.mutation_seq().get(), 1);
    assert_eq!(stored.meta(), &Meta::default());
    let entries = tracker.entries().await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].lifetime(), retrack.new_lifetime);
    assert_eq!(entries[0].stored_ref(), &retrack.new_ref);
    Ok(())
}

#[tokio::test]
async fn foreign_removal_is_retracked_with_a_fresh_lifetime_and_ref() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let mut events = presence.events();
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;
    let ours = applied(tracker.track(&topic, &key, Meta::default()).await?);

    let store = jetstream::new(client.clone())
        .get_key_value(config.bucket().to_string())
        .await?;
    store.purge(entry.encode(config.shards())?.as_str()).await?;

    let retrack = next_retrack(&mut events, Duration::from_secs(3)).await?;
    assert_eq!(retrack.old_lifetime, ours.lifetime());
    assert_ne!(retrack.new_lifetime, ours.lifetime());
    assert_ne!(&retrack.new_ref, ours.stored_ref());
    let stored = StoredValue::from_json_bytes(&raw_entry(&client, &config, &entry).await?.payload)?;
    assert_eq!(stored.lifetime(), retrack.new_lifetime);
    assert_eq!(stored.phx_ref(), &retrack.new_ref);
    Ok(())
}

#[tokio::test]
async fn heartbeat_timeout_never_publishes_expected_sequence_zero() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = fast_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let key: PresenceKey = "ana".parse()?;
    let tracker = presence.tracker()?;
    let mut entries = Vec::new();
    for index in 0..5 {
        let topic: Topic = format!("room:{index}").parse()?;
        applied(tracker.track(&topic, &key, Meta::default()).await?);
        entries.push(EntryKey::new(topic, key.clone(), *tracker.holder()));
    }

    let observer = server.client().await;
    let mut published = observer.subscribe(format!("$KV.{}.>", config.bucket())).await?;
    observer.flush().await?;
    let prefix = format!("$KV.{}.", config.bucket());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let watcher = tokio::spawn(async move {
        while let Some(message) = futures_util::StreamExt::next(&mut published).await {
            let Some(rest) = message.subject.as_str().strip_prefix(prefix.as_str()) else {
                continue;
            };
            let shard = rest.split('.').next().unwrap_or_default();
            if !(shard.starts_with('s') && shard[1..].chars().all(|c| c.is_ascii_digit())) {
                continue;
            }
            let expected = message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE.as_ref()))
                .map(|value| value.as_str().to_owned());
            if let Ok(mut recorded) = recorded.lock() {
                recorded.push((message.subject.to_string(), expected));
            }
        }
    });

    tokio::time::sleep(Duration::from_millis(600)).await;
    server.pause();
    tokio::time::sleep(Duration::from_secs(3)).await;
    server.resume();
    tokio::time::sleep(Duration::from_secs(4)).await;

    for entry in &entries {
        let raw = raw_entry(&client, &config, entry).await?;
        assert_eq!(
            header(&raw, NATS_MARKER_REASON.as_ref()),
            None,
            "entry was not kept alive"
        );
        StoredValue::from_json_bytes(&raw.payload)?;
    }
    assert_eq!(tracker.entries().await?.len(), entries.len());
    watcher.abort();
    let seen = seen.lock().map_err(|_| "observer poisoned")?.clone();
    assert!(!seen.is_empty(), "no data publish was observed");
    let zero: Vec<_> = seen
        .iter()
        .filter(|(_, expected)| expected.as_deref() == Some("0"))
        .collect();
    assert!(
        zero.is_empty(),
        "data subjects published with expected sequence 0: {zero:?}"
    );
    Ok(())
}

#[tokio::test]
async fn untrack_during_a_round_is_not_resurrected() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = resilient_untrack_config()?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let mut stream = jetstream::new(client.clone())
        .get_stream(config.bucket().stream_name())
        .await?;
    let key: PresenceKey = "ana".parse()?;

    for (round, offset_ms) in [0u64, 90, 180, 270, 360].into_iter().enumerate() {
        let tracker = presence.tracker()?;
        let mut entries = Vec::new();
        for index in 0..40 {
            let topic: Topic = format!("room:{round}:{index}").parse()?;
            tracker.track(&topic, &key, Meta::default()).await?;
            entries.push(EntryKey::new(topic, key.clone(), *tracker.holder()));
        }
        tokio::time::sleep(Duration::from_millis(800 + offset_ms)).await;
        assert!(tracker.untrack_all().await?.is_complete());
        let untracked_at = stream.info().await?.state.last_sequence;
        tokio::time::sleep(Duration::from_millis(1200)).await;

        assert!(tracker.entries().await?.is_empty());
        for entry in &entries {
            let raw = match last_record_or_reclaimed(&client, &config, entry).await? {
                LastRecord::Present(raw) => raw,
                LastRecord::Reclaimed => continue,
            };
            assert!(
                StoredValue::from_json_bytes(&raw.payload).is_err(),
                "entry was resurrected: seq {} headers {:?} payload {:?}",
                raw.sequence,
                raw.headers,
                String::from_utf8_lossy(&raw.payload)
            );
            assert!(
                raw.sequence <= untracked_at,
                "key was written after untrack completed at {untracked_at}: seq {} headers {:?}",
                raw.sequence,
                raw.headers
            );
            let purged = header(&raw, "KV-Operation") == Some("PURGE");
            let expired_before_untrack = header(&raw, NATS_MARKER_REASON.as_ref()) == Some("MaxAge");
            assert!(
                purged || expired_before_untrack,
                "unexpected last message: seq {} headers {:?}",
                raw.sequence,
                raw.headers
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn three_hundred_entries_heartbeat_within_one_interval() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let interval = Duration::from_millis(800);
    let config = config(Duration::from_secs(2), interval, DEFAULT_MARKER_TTL)?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let key: PresenceKey = "ana".parse()?;

    let mut tracking = JoinSet::new();
    for shard in 0..10 {
        let tracker = presence.tracker()?;
        let key = key.clone();
        tracking.spawn(async move {
            let mut entries = Vec::new();
            for index in 0..30 {
                let topic: Topic = format!("room:{shard}:{index}").parse()?;
                tracker.track(&topic, &key, Meta::default()).await?;
                entries.push(EntryKey::new(topic, key.clone(), *tracker.holder()));
            }
            Ok::<_, TestError>((tracker, entries))
        });
    }
    let mut trackers = Vec::new();
    let mut entries = Vec::new();
    while let Some(joined) = tracking.join_next().await {
        let (tracker, tracked) = joined??;
        trackers.push(tracker);
        entries.extend(tracked);
    }
    assert_eq!(entries.len(), 300);

    tokio::time::sleep(interval * 3).await;
    presence.close();

    let mut stamps = Vec::with_capacity(entries.len());
    for entry in &entries {
        let raw = raw_entry(&client, &config, entry).await?;
        assert_eq!(header(&raw, NATS_MARKER_REASON.as_ref()), None, "entry expired");
        assert_ne!(
            header(&raw, NATS_EXPECTED_LAST_SUBJECT_SEQUENCE.as_ref()),
            Some("0"),
            "entry was never heartbeated"
        );
        stamps.push(raw.time);
    }
    stamps.sort();

    let split = i128::from(u32::try_from(interval.as_millis() / 2)?);
    let mut group_start = stamps[0];
    let mut previous = stamps[0];
    for stamp in stamps.iter().copied().skip(1) {
        if (stamp - previous).whole_milliseconds() > split {
            group_start = stamp;
        }
        let span = (stamp - group_start).whole_milliseconds();
        assert!(
            span < i128::from(u32::try_from(interval.as_millis())?),
            "a round took {span} ms"
        );
        previous = stamp;
    }
    drop(trackers);
    Ok(())
}
