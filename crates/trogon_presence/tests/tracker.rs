mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use async_nats::header::{NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, NATS_MARKER_REASON, NATS_MESSAGE_TTL};
use async_nats::jetstream::{self, kv, message::StreamMessage};
use async_nats::HeaderMap;
use common::NatsServer;
use trogon_presence::value::{StoredValue, Tombstone, ValueError};
use trogon_presence::{
    BucketField, BucketName, EntryKey, EntryTarget, HeartbeatInterval, HolderId, Incompatible, LeaseTtl, Liveness,
    MarkerTtl, Meta, OpenError, OperationResult, Presence, PresenceConfig, PresenceKey, ProvisionError,
    ProvisionOptions, ShardCount, Topic, TrackerError, WriteError, WriteOutcome, WriteReceipt, WriterMode,
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

fn meta(status: &str) -> Result<Meta, serde_json::Error> {
    serde_json::from_value(serde_json::json!({ "status": status }))
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

async fn last_sequence(client: &async_nats::Client, config: &PresenceConfig) -> Result<u64, TestError> {
    let mut stream = jetstream::new(client.clone())
        .get_stream(config.bucket().stream_name())
        .await?;
    Ok(stream.info().await?.state.last_sequence)
}

fn header<'a>(message: &'a StreamMessage, name: &str) -> Option<&'a str> {
    message.headers.get(name).map(|value| value.as_str())
}

fn identity() -> Result<(Topic, PresenceKey, HolderId), TestError> {
    Ok(("room:lobby".parse()?, "ana@x.io".parse()?, HolderId::generate()?))
}

fn applied(outcome: WriteOutcome) -> WriteReceipt {
    match outcome {
        WriteOutcome::Applied(receipt) => receipt,
        other => panic!("expected an applied write, got {other:?}"),
    }
}

fn target(receipt: &WriteReceipt) -> EntryTarget {
    EntryTarget::new(receipt.lifetime(), receipt.sequence())
}

#[tokio::test]
async fn track_update_untrack_write_expected_headers_and_values() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);

    let first = applied(tracker.track(&topic, &key, meta("online")?).await?);
    assert_eq!(first.sequence().get(), 1);
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(raw.sequence, first.revision().get());
    assert_eq!(header(&raw, NATS_MESSAGE_TTL.as_ref()), Some("30s"));
    assert_eq!(header(&raw, NATS_EXPECTED_LAST_SUBJECT_SEQUENCE.as_ref()), Some("0"));
    let stored = StoredValue::from_json_bytes(&raw.payload)?;
    assert_eq!(stored.topic(), &topic);
    assert_eq!(stored.key(), &key);
    assert_eq!(stored.phx_ref(), first.stored_ref());
    assert_eq!(stored.phx_ref_prev(), None);
    assert_eq!(stored.lifetime(), first.lifetime());
    assert_eq!(stored.mutation_seq(), first.sequence());
    assert_eq!(stored.last_op().outcome(), OperationResult::Tracked);
    assert_eq!(stored.meta(), &meta("online")?);
    assert_eq!(stored.client_meta(), &meta("online")?);

    let second = applied(tracker.update(&topic, &key, meta("away")?).await?);
    assert_ne!(second.stored_ref(), first.stored_ref());
    assert_eq!(second.lifetime(), first.lifetime());
    assert_eq!(second.sequence().get(), 2);
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(raw.sequence, second.revision().get());
    assert_eq!(header(&raw, NATS_MESSAGE_TTL.as_ref()), Some("30s"));
    let expected_prev = first.revision().to_string();
    assert_eq!(
        header(&raw, NATS_EXPECTED_LAST_SUBJECT_SEQUENCE.as_ref()),
        Some(expected_prev.as_str())
    );
    let stored = StoredValue::from_json_bytes(&raw.payload)?;
    assert_eq!(stored.phx_ref(), second.stored_ref());
    assert_eq!(stored.phx_ref_prev(), Some(first.stored_ref()));
    assert_eq!(stored.meta(), &meta("away")?);
    assert_eq!(stored.birth_rev(), Some(first.revision()));

    let entries = tracker.entries().await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].revision(), second.revision());
    assert_eq!(entries[0].stored_ref(), second.stored_ref());
    assert_eq!(entries[0].birth(), first.revision());

    let third = applied(tracker.untrack(&topic, &key).await?);
    assert_eq!(third.sequence().get(), 3);
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(header(&raw, "KV-Operation"), Some("PURGE"));
    assert_eq!(header(&raw, NATS_MESSAGE_TTL.as_ref()), Some("300s"));
    let expected_prev = second.revision().to_string();
    assert_eq!(
        header(&raw, NATS_EXPECTED_LAST_SUBJECT_SEQUENCE.as_ref()),
        Some(expected_prev.as_str())
    );
    let tombstone = Tombstone::from_json_bytes(&raw.payload)?;
    assert_eq!(tombstone.lifetime(), first.lifetime());
    assert_eq!(tombstone.mutation_seq().get(), 3);
    assert_eq!(tombstone.last_op().outcome(), OperationResult::Untracked);
    assert!(tracker.entries().await?.is_empty());
    assert_eq!(tracker.untrack(&topic, &key).await?, WriteOutcome::Gone);
    Ok(())
}

#[tokio::test]
async fn identical_retry_replays_without_writing() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    presence.close();
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;
    let intent = tracker.request()?.track(topic.clone(), key.clone(), meta("online")?);

    let first = applied(tracker.submit(intent.clone(), None).await?);
    let before = last_sequence(&client, &config).await?;
    assert_eq!(
        tracker.submit(intent, None).await?,
        WriteOutcome::Replayed {
            receipt: first,
            liveness: Liveness::Live
        }
    );
    assert_eq!(last_sequence(&client, &config).await?, before);
    Ok(())
}

#[tokio::test]
async fn same_operation_with_other_meta_conflicts() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    presence.close();
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;
    let request = tracker.request()?;

    applied(
        tracker
            .submit(request.track(topic.clone(), key.clone(), meta("online")?), None)
            .await?,
    );
    let before = last_sequence(&client, &config).await?;
    let result = tracker
        .submit(request.track(topic.clone(), key.clone(), meta("away")?), None)
        .await;
    assert!(
        matches!(result, Err(WriteError::OperationConflict)),
        "unexpected result: {result:?}"
    );
    assert_eq!(last_sequence(&client, &config).await?, before);
    Ok(())
}

#[tokio::test]
async fn delayed_track_after_untrack_replays_without_recreating() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    presence.close();
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;
    let track = tracker.request()?.track(topic.clone(), key.clone(), meta("online")?);

    let tracked = applied(tracker.submit(track.clone(), None).await?);
    applied(tracker.untrack(&topic, &key).await?);
    let purged = raw_entry(&client, &config, &entry).await?;

    assert_eq!(
        tracker.submit(track, None).await?,
        WriteOutcome::Replayed {
            receipt: tracked,
            liveness: Liveness::Retired
        }
    );
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(raw.sequence, purged.sequence);
    assert_eq!(header(&raw, "KV-Operation"), Some("PURGE"));
    assert!(tracker.entries().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn update_naming_a_retired_lifetime_is_gone_or_conflicts() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    presence.close();
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;

    let retired = applied(tracker.track(&topic, &key, meta("online")?).await?);
    applied(tracker.untrack(&topic, &key).await?);
    let stale = tracker
        .request()?
        .update(topic.clone(), key.clone(), target(&retired), meta("away")?);
    assert_eq!(tracker.submit(stale, None).await?, WriteOutcome::Gone);

    let current = applied(tracker.track(&topic, &key, meta("back")?).await?);
    assert_ne!(current.lifetime(), retired.lifetime());
    let stale = tracker
        .request()?
        .update(topic.clone(), key.clone(), target(&retired), meta("away")?);
    let result = tracker.submit(stale, None).await;
    let Err(WriteError::Conflict { current_ref, .. }) = result else {
        panic!("expected a conflict, got {result:?}");
    };
    assert_eq!(&current_ref, current.stored_ref());
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(raw.sequence, current.revision().get());
    Ok(())
}

#[tokio::test]
async fn update_naming_an_old_sequence_conflicts() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let presence = Presence::provision(client, PresenceConfig::default(), ProvisionOptions::default()).await?;
    presence.close();
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;

    let first = applied(tracker.track(&topic, &key, meta("online")?).await?);
    let second = applied(tracker.update(&topic, &key, meta("away")?).await?);
    let stale = tracker
        .request()?
        .update(topic.clone(), key.clone(), target(&first), meta("busy")?);
    let result = tracker.submit(stale, None).await;
    let Err(WriteError::SequenceConflict { current }) = result else {
        panic!("expected a sequence conflict, got {result:?}");
    };
    assert_eq!(current, second.sequence());
    Ok(())
}

#[tokio::test]
async fn noop_update_keeps_the_ref_and_advances_the_sequence() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    presence.close();
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;

    let first = applied(tracker.track(&topic, &key, meta("online")?).await?);
    let same = applied(tracker.update(&topic, &key, meta("online")?).await?);
    assert_eq!(same.stored_ref(), first.stored_ref());
    assert_eq!(same.lifetime(), first.lifetime());
    assert_eq!(same.sequence().get(), 2);
    assert!(same.revision() > first.revision());
    let stored = StoredValue::from_json_bytes(&raw_entry(&client, &config, &entry).await?.payload)?;
    assert_eq!(stored.phx_ref(), first.stored_ref());
    assert_eq!(stored.phx_ref_prev(), None);
    assert_eq!(stored.last_op().outcome(), OperationResult::Updated);

    let changed = applied(tracker.update(&topic, &key, meta("away")?).await?);
    assert_ne!(changed.stored_ref(), first.stored_ref());
    assert_eq!(changed.sequence().get(), 3);
    let stored = StoredValue::from_json_bytes(&raw_entry(&client, &config, &entry).await?.payload)?;
    assert_eq!(stored.phx_ref(), changed.stored_ref());
    assert_eq!(stored.phx_ref_prev(), Some(first.stored_ref()));
    Ok(())
}

#[tokio::test]
async fn track_twice_returns_already_live() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let presence = Presence::provision(client, PresenceConfig::default(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;

    let first = applied(tracker.track(&topic, &key, meta("online")?).await?);
    assert_eq!(
        tracker.track(&topic, &key, meta("online")?).await?,
        WriteOutcome::AlreadyLive(first)
    );
    Ok(())
}

#[tokio::test]
async fn tracker_for_returns_the_live_handle() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let presence = Presence::provision(client, PresenceConfig::default(), ProvisionOptions::default()).await?;
    let holder = HolderId::generate()?;
    let first = presence.tracker_for(holder)?;
    let second = presence.tracker_for(holder)?;
    assert_eq!(first, second);
    assert_ne!(first, presence.tracker()?);
    Ok(())
}

#[tokio::test]
async fn two_processes_contend_on_the_direct_guard() -> TestResult {
    let server = server_or_skip!();
    let config = PresenceConfig::default();
    let first = Presence::provision(server.client().await, config.clone(), ProvisionOptions::default()).await?;
    let second = Presence::open(server.client().await, config).await?;
    let (topic, key, holder) = identity()?;

    let winner = first.tracker_for(holder)?;
    let loser = second.tracker_for(holder)?;
    applied(winner.track(&topic, &key, meta("online")?).await?);
    let result = loser.track(&"room:other".parse()?, &key, meta("online")?).await;
    assert!(
        matches!(result, Err(WriteError::HolderBusy(ref busy)) if busy.holder() == holder),
        "unexpected result: {result:?}"
    );
    assert!(loser.entries().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn envelope_above_the_cap_is_too_large_and_not_written() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;
    let big: Meta = serde_json::from_value(serde_json::json!({ "bio": "x".repeat(2_100) }))?;

    let before = last_sequence(&client, &config).await?;
    let result = tracker.track(&topic, &key, big).await;
    assert!(
        matches!(result, Err(WriteError::Value(ValueError::TooLarge { .. }))),
        "unexpected result: {result:?}"
    );
    assert_eq!(last_sequence(&client, &config).await?, before);
    assert!(tracker.entries().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn live_value_without_a_receipt_is_adopted() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let payload = format!(
        r#"{{"v":2,"topic":"room:lobby","key":"ana@x.io","phx_ref":"Fq1","phx_ref_prev":null,"meta":{{"status":"online"}},"lifetime":"AAAAAAAAAAAAAAAAAAAAAA","mutation_seq":"1","last_op":{{"id":"AQEBAQEBAQEBAQEBAQEBAQ","fingerprint":"{}","outcome":"tracked"}},"client_meta":{{"status":"online"}},"birth_rev":null}}"#,
        "A".repeat(43)
    );
    let value = StoredValue::from_json_bytes(payload.as_bytes())?;

    let mut headers = HeaderMap::new();
    headers.insert(NATS_MESSAGE_TTL, config.lease_ttl().header_value());
    headers.insert(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, "0");
    let subject = config.bucket().subject_for(&entry.encode(config.shards())?);
    let landed = jetstream::new(client)
        .publish_with_headers(subject, headers, value.to_json_bytes()?.into())
        .await?
        .await?;

    let tracker = presence.tracker_for(holder)?;
    let WriteOutcome::AlreadyLive(adopted) = tracker.track(&topic, &key, meta("online")?).await? else {
        panic!("a live value must be adopted");
    };
    assert_eq!(adopted.stored_ref(), value.phx_ref());
    assert_eq!(adopted.lifetime(), value.lifetime());
    assert_eq!(adopted.revision().get(), landed.sequence);
    let updated = applied(tracker.update(&topic, &key, meta("away")?).await?);
    assert_eq!(updated.lifetime(), value.lifetime());
    assert_eq!(updated.sequence().get(), 2);
    Ok(())
}

#[tokio::test]
async fn expiry_marker_makes_update_gone_and_retrack_keeps_ttl() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::new(
        BucketName::default(),
        LeaseTtl::try_from(Duration::from_secs(1))?,
        HeartbeatInterval::try_from(Duration::from_millis(400))?,
        MarkerTtl::try_from(Duration::from_secs(5))?,
        ShardCount::DEFAULT,
    )?;
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let tracker = presence.tracker_for(holder)?;
    presence.close();

    let expired = applied(tracker.track(&topic, &key, meta("online")?).await?);
    let deadline = Instant::now() + Duration::from_secs(10);
    let marker = loop {
        let raw = raw_entry(&client, &config, &entry).await?;
        if header(&raw, NATS_MARKER_REASON.as_ref()).is_some() {
            break raw;
        }
        assert!(Instant::now() < deadline, "entry never expired");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(header(&marker, NATS_MARKER_REASON.as_ref()), Some("MaxAge"));

    assert_eq!(tracker.update(&topic, &key, meta("away")?).await?, WriteOutcome::Gone);
    assert!(tracker.entries().await?.is_empty());

    let fresh = applied(tracker.track(&topic, &key, meta("back")?).await?);
    assert_ne!(fresh.lifetime(), expired.lifetime());
    assert_ne!(fresh.stored_ref(), expired.stored_ref());
    assert!(fresh.revision().get() > marker.sequence);
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(header(&raw, NATS_MESSAGE_TTL.as_ref()), Some("1s"));
    let marker_seq = marker.sequence.to_string();
    assert_eq!(
        header(&raw, NATS_EXPECTED_LAST_SUBJECT_SEQUENCE.as_ref()),
        Some(marker_seq.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn untrack_all_purges_every_entry() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let presence = Presence::provision(client, PresenceConfig::default(), ProvisionOptions::default()).await?;
    let tracker = presence.tracker()?;
    let key: PresenceKey = "ana".parse()?;
    for topic in ["room:a", "room:b", "room:c"] {
        applied(tracker.track(&topic.parse()?, &key, Meta::default()).await?);
    }
    assert_eq!(tracker.entries().await?.len(), 3);
    let report = tracker.untrack_all().await?;
    assert_eq!(report.outcomes().len(), 3);
    assert!(report.is_complete());
    assert!(report
        .outcomes()
        .iter()
        .all(|outcome| matches!(outcome.result(), Ok(WriteOutcome::Applied(_)))));
    assert!(tracker.entries().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn open_rejects_bucket_with_history_two() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    jetstream::new(client.clone())
        .create_key_value(kv::Config {
            bucket: config.bucket().to_string(),
            history: 2,
            limit_markers: Some(config.marker_ttl().get()),
            ..Default::default()
        })
        .await?;
    let result = Presence::open(client, config).await;
    assert!(
        matches!(
            result,
            Err(OpenError::Incompatible(Incompatible {
                field: BucketField::History,
                ..
            }))
        ),
        "unexpected open result: {:?}",
        result.err()
    );
    Ok(())
}

#[tokio::test]
async fn open_validates_existence_and_shard_count() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    assert!(matches!(
        Presence::open(client.clone(), config.clone()).await.err(),
        Some(OpenError::BucketMissing(_))
    ));

    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    assert!(Presence::open(client.clone(), config.clone()).await.is_ok());

    let other_shards = PresenceConfig::new(
        config.bucket().clone(),
        config.lease_ttl(),
        config.heartbeat(),
        config.marker_ttl(),
        ShardCount::try_from(128)?,
    )?;
    assert!(matches!(
        Presence::open(client.clone(), other_shards.clone()).await.err(),
        Some(OpenError::Incompatible(Incompatible {
            field: BucketField::ShardCount,
            ..
        }))
    ));
    assert!(Presence::provision(client.clone(), config, ProvisionOptions::default())
        .await
        .is_ok());
    let conflicting = Presence::provision(client, other_shards, ProvisionOptions::default()).await;
    assert!(
        matches!(
            conflicting,
            Err(ProvisionError::Incompatible(Incompatible {
                field: BucketField::ShardCount,
                ..
            }))
        ),
        "unexpected provision result: {:?}",
        conflicting.err()
    );
    Ok(())
}

#[tokio::test]
async fn update_with_a_stale_expected_ref_conflicts_without_writing() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let first = applied(tracker.track(&topic, &key, meta("online")?).await?);
    let second = applied(tracker.update(&topic, &key, meta("away")?).await?);

    let result = tracker
        .update_with_expected_ref(&topic, &key, first.stored_ref(), meta("busy")?)
        .await;
    let Err(WriteError::Conflict {
        current_ref,
        current_meta,
    }) = result
    else {
        panic!("expected a conflict, got {result:?}");
    };
    assert_eq!(&current_ref, second.stored_ref());
    assert_eq!(current_meta, meta("away")?);

    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(raw.sequence, second.revision().get());
    Ok(())
}

#[tokio::test]
async fn update_with_the_current_expected_ref_writes_a_successor() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let (topic, key, holder) = identity()?;
    let tracker = presence.tracker_for(holder)?;
    let entry = EntryKey::new(topic.clone(), key.clone(), holder);
    let first = applied(tracker.track(&topic, &key, meta("online")?).await?);

    let next = applied(
        tracker
            .update_with_expected_ref(&topic, &key, first.stored_ref(), meta("away")?)
            .await?,
    );
    assert_ne!(next.stored_ref(), first.stored_ref());
    let raw = raw_entry(&client, &config, &entry).await?;
    assert_eq!(raw.sequence, next.revision().get());
    let stored = StoredValue::from_json_bytes(&raw.payload)?;
    assert_eq!(stored.phx_ref(), next.stored_ref());
    assert_eq!(stored.phx_ref_prev(), Some(first.stored_ref()));
    assert_eq!(stored.meta(), &meta("away")?);
    Ok(())
}

#[tokio::test]
async fn a_managed_bucket_refuses_direct_trackers() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default().with_writer_mode(WriterMode::Managed);
    let presence = Presence::provision(client, config.clone(), ProvisionOptions::default()).await?;
    assert_eq!(presence.writer_mode(), WriterMode::Managed);
    for refused in [
        presence.tracker().err(),
        presence.tracker_for(HolderId::generate()?).err(),
    ] {
        match refused {
            Some(TrackerError::WriterMode {
                bucket,
                expected,
                found,
            }) => {
                assert_eq!(&bucket, config.bucket());
                assert_eq!(expected, WriterMode::Direct);
                assert_eq!(found, WriterMode::Managed);
            }
            other => panic!("expected a writer mode refusal, got {other:?}"),
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_direct_bucket_hands_out_trackers_and_refuses_managed_openers() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    assert_eq!(presence.writer_mode(), WriterMode::Direct);
    let (topic, key, holder) = identity()?;
    applied(
        presence
            .tracker_for(holder)?
            .track(&topic, &key, Meta::default())
            .await?,
    );
    applied(presence.tracker()?.track(&topic, &key, Meta::default()).await?);

    let managed = Presence::open(client, config.with_writer_mode(WriterMode::Managed)).await;
    assert!(
        matches!(
            managed,
            Err(OpenError::Incompatible(Incompatible {
                field: BucketField::WriterMode,
                ..
            }))
        ),
        "a managed opener was not refused"
    );
    Ok(())
}

#[tokio::test]
async fn untrack_all_reports_every_entry_when_some_batches_fail() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let tracker = presence.tracker()?;
    let key: PresenceKey = "ana".parse()?;
    let mut doomed = HashSet::new();
    for index in 0..3 {
        let topic: Topic = format!("room:{index}{}:{}", "l".repeat(120), "m".repeat(120)).parse()?;
        applied(tracker.track(&topic, &key, Meta::default()).await?);
        doomed.insert(EntryKey::new(topic, key.clone(), *tracker.holder()));
    }
    for index in 0..67 {
        applied(
            tracker
                .track(&format!("room:s{index}").parse()?, &key, Meta::default())
                .await?,
        );
    }
    let total = 70;
    assert_eq!(tracker.entries().await?.len(), total);

    let context = jetstream::new(client);
    let mut stream_config = context
        .get_stream(config.bucket().stream_name())
        .await?
        .cached_info()
        .config
        .clone();
    stream_config.max_message_size = 512;
    context.update_stream(stream_config).await?;

    let report = tracker.untrack_all().await?;
    assert_eq!(report.outcomes().len(), total);
    assert!(!report.is_complete());
    let failed: HashSet<EntryKey> = report
        .outcomes()
        .iter()
        .filter(|outcome| outcome.result().is_err())
        .map(|outcome| outcome.entry().clone())
        .collect();
    assert_eq!(failed, doomed);
    assert!(report
        .outcomes()
        .iter()
        .filter_map(|outcome| outcome.result().as_ref().err())
        .all(|err| matches!(err, WriteError::Rejected(_))));
    assert!(report
        .outcomes()
        .iter()
        .filter(|outcome| outcome.result().is_ok())
        .all(|outcome| matches!(outcome.result(), Ok(WriteOutcome::Applied(_)))));

    let remaining: HashSet<EntryKey> = tracker
        .entries()
        .await?
        .iter()
        .map(|entry| entry.entry().clone())
        .collect();
    assert_eq!(remaining, doomed);
    Ok(())
}
