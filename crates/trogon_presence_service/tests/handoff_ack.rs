mod common;

use std::collections::BTreeMap;
use std::error::Error;
use std::time::{Duration, Instant};

use async_nats::jetstream;
use async_nats::jetstream::stream::LastRawMessageErrorKind;
use futures_util::future::join_all;
use futures_util::StreamExt;
use serde_json::{json, Value};
use trogon_presence::value::StoredValue;
use trogon_presence::{
    EntryKey, HolderId, PresenceConfig, PresenceKey, ProvisionOptions, ShardCount, Topic, WriterMode,
};
use trogon_presence_service::{
    KeepaliveInterval, NodeId, ReplyCode, ServiceConfig, ServiceHandle, ShardLeaseTtl, WriteOp,
};

use common::{Access, NatsServer};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type BoxError = Box<dyn Error + Send + Sync>;

const WRITERS: usize = 200;
const ROOMS: usize = 10;
const LOBBY: &str = "room:lobby";
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const OWNERSHIP_TIMEOUT: Duration = Duration::from_secs(30);
const HANDOFF_WITHIN: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(20);

/// The FIND-013 signature: an ingress permit held by a denied hand-off ack, never a starved writer.
const INGRESS_QUEUE_FULL: &str = "ingress queue is full";
const OVERLOAD_RETRY_ATTEMPTS: u32 = 10;
const OVERLOAD_RETRY_BASE: Duration = Duration::from_millis(20);
const OVERLOAD_RETRY_CAP: Duration = Duration::from_millis(500);
const OVERLOAD_RETRY_DEADLINE: Duration = Duration::from_secs(15);
const CONFIRM_DEADLINE: Duration = Duration::from_secs(15);

macro_rules! restricted_server_or_skip {
    () => {
        match NatsServer::start_with(Access::RuntimeDeniedPlainInbox).await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn service_config() -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::new(
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ShardCount::DEFAULT,
    )?
    .with_writer_mode(WriterMode::Managed);
    Ok(ServiceConfig::new(presence, "handoff".parse::<NodeId>()?)
        .with_lease_ttl(ShardLeaseTtl::try_from(Duration::from_secs(3))?)
        .with_keepalive(KeepaliveInterval::try_from(Duration::from_secs(1))?))
}

async fn start_owning_all(server: &NatsServer) -> Result<ServiceHandle, BoxError> {
    let config = service_config()?;
    let client = server.runtime_client().await;
    trogon_presence_service::provision(client.clone(), &config, ProvisionOptions::default()).await?;
    let handle = trogon_presence_service::start(client, config).await?;
    let shards = usize::from(ShardCount::DEFAULT.get());
    tokio::time::timeout(OWNERSHIP_TIMEOUT, async {
        while handle.owned_shards().len() != shards {
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .map_err(|_| "the service did not take every view shard")?;
    common::wait_for_writers(&handle, shards, OWNERSHIP_TIMEOUT).await?;
    Ok(handle)
}

#[derive(Clone)]
struct Reply {
    code: String,
    body: Value,
}

async fn send(client: &async_nats::Client, subject: String, key: &PresenceKey, body: Value) -> Result<Reply, BoxError> {
    let inbox = common::caller_inbox(key)?;
    let mut replies = client.subscribe(inbox.clone()).await?;
    client
        .publish_with_reply(subject, inbox, serde_json::to_vec(&body)?.into())
        .await?;
    client.flush().await?;
    let message = tokio::time::timeout(REPLY_TIMEOUT, replies.next())
        .await
        .map_err(|_| format!("no reply for {key}"))?
        .ok_or("reply subscription ended")?;
    Ok(Reply {
        code: common::code_of(&message).unwrap_or("none").to_owned(),
        body: common::body_of(&message)?,
    })
}

/// Full jitter backoff (AWS-style): `random(0, min(cap, base * 2^attempt))`.
fn full_jitter_backoff(attempt: u32) -> Duration {
    let base = OVERLOAD_RETRY_BASE.as_millis() as u64;
    let cap = OVERLOAD_RETRY_CAP.as_millis() as u64;
    let ceiling = base.saturating_mul(1u64 << attempt.min(16)).min(cap).max(1);
    Duration::from_millis(getrandom::u64().map_or(ceiling, |random| random % ceiling))
}

/// Retries a command while the admission layer answers `overloaded`, bounded by both an attempt
/// count and a wall-clock deadline, and records every attempted reply (not only the final one).
async fn send_retrying_overload(
    client: &async_nats::Client,
    subject: &str,
    key: &PresenceKey,
    body: &Value,
    attempts: &mut Vec<Reply>,
) -> Result<Reply, BoxError> {
    let deadline = Instant::now() + OVERLOAD_RETRY_DEADLINE;
    let mut attempt = 0u32;
    loop {
        let reply = send(client, subject.to_owned(), key, body.clone()).await?;
        let overloaded = reply.code == ReplyCode::Overloaded.as_str();
        attempts.push(reply.clone());
        attempt += 1;
        if !overloaded || attempt >= OVERLOAD_RETRY_ATTEMPTS || Instant::now() >= deadline {
            return Ok(reply);
        }
        tokio::time::sleep(full_jitter_backoff(attempt)).await;
    }
}

fn is_reply_deadline_unknown(reply: &Reply) -> bool {
    reply.code == ReplyCode::Unavailable.as_str()
        && reply.body["outcome_unknown"] == true
        && reply.body["retryable"] == true
}

enum RecordState {
    Tracked(Box<StoredValue>),
    Untracked,
}

async fn record_state(
    client: &async_nats::Client,
    presence: &PresenceConfig,
    entry: &EntryKey,
) -> Result<RecordState, BoxError> {
    let stream = jetstream::new(client.clone())
        .get_stream(presence.bucket().stream_name())
        .await?;
    let subject = presence.bucket().subject_for(&entry.encode(presence.shards())?);
    match stream.get_last_raw_message_by_subject(&subject).await {
        Ok(message) => match StoredValue::from_json_bytes(&message.payload) {
            Ok(value) => Ok(RecordState::Tracked(Box::new(value))),
            Err(_) => Ok(RecordState::Untracked),
        },
        Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(RecordState::Untracked),
        Err(err) => Err(err.into()),
    }
}

/// Polls the stored record for a writer whose reply deadline fired before its job finished,
/// so a degraded acknowledgement is only accepted once the real end state is confirmed.
async fn confirm_tracked(
    client: &async_nats::Client,
    presence: &PresenceConfig,
    entry: &EntryKey,
) -> Result<Option<Box<StoredValue>>, BoxError> {
    let deadline = Instant::now() + CONFIRM_DEADLINE;
    loop {
        if let RecordState::Tracked(value) = record_state(client, presence, entry).await? {
            return Ok(Some(value));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn confirm_untracked(
    client: &async_nats::Client,
    presence: &PresenceConfig,
    entry: &EntryKey,
) -> Result<bool, BoxError> {
    let deadline = Instant::now() + CONFIRM_DEADLINE;
    loop {
        if matches!(record_state(client, presence, entry).await?, RecordState::Untracked) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

struct Entry {
    key: PresenceKey,
    topic: Topic,
    holder: HolderId,
    tracked: Value,
}

fn topics_of(writer: usize) -> Result<[Topic; 2], BoxError> {
    Ok([LOBBY.parse()?, format!("room:{}", writer % ROOMS).parse()?])
}

struct TrackOutcome {
    entries: Vec<Entry>,
    failures: Vec<Reply>,
    attempts: Vec<Reply>,
}

async fn track(
    client: &async_nats::Client,
    presence: &PresenceConfig,
    writer: usize,
) -> Result<TrackOutcome, BoxError> {
    let key: PresenceKey = format!("writer{writer}").parse()?;
    let holder = HolderId::generate()?;
    let mut entries = Vec::new();
    let mut failures = Vec::new();
    let mut attempts = Vec::new();
    for topic in topics_of(writer)? {
        let body = json!({ "holder": holder, "meta": { "status": "online" } });
        let reply = send_retrying_overload(
            client,
            &WriteOp::Track.subject(&key, &topic),
            &key,
            &body,
            &mut attempts,
        )
        .await?;
        if reply.code == "ok" {
            entries.push(Entry {
                key: key.clone(),
                topic,
                holder,
                tracked: reply.body,
            });
            continue;
        }
        if is_reply_deadline_unknown(&reply) {
            let entry_key = EntryKey::new(topic.clone(), key.clone(), holder);
            if let Some(value) = confirm_tracked(client, presence, &entry_key).await? {
                entries.push(Entry {
                    key: key.clone(),
                    topic,
                    holder,
                    tracked: json!({
                        "lifetime": serde_json::to_value(value.lifetime())?,
                        "mutation_seq": serde_json::to_value(value.mutation_seq())?,
                    }),
                });
                continue;
            }
        }
        failures.push(reply);
    }
    Ok(TrackOutcome {
        entries,
        failures,
        attempts,
    })
}

struct UntrackOutcome {
    failures: Vec<Reply>,
    attempts: Vec<Reply>,
}

async fn untrack(
    client: &async_nats::Client,
    presence: &PresenceConfig,
    entries: &[Entry],
) -> Result<UntrackOutcome, BoxError> {
    let mut failures = Vec::new();
    let mut attempts = Vec::new();
    for entry in entries {
        let body = json!({
            "holder": entry.holder,
            "lifetime": entry.tracked["lifetime"],
            "mutation_seq": entry.tracked["mutation_seq"],
        });
        let subject = WriteOp::Untrack.subject(&entry.key, &entry.topic);
        let reply = send_retrying_overload(client, &subject, &entry.key, &body, &mut attempts).await?;
        if reply.code == "ok" {
            continue;
        }
        if is_reply_deadline_unknown(&reply) {
            let entry_key = EntryKey::new(entry.topic.clone(), entry.key.clone(), entry.holder);
            if confirm_untracked(client, presence, &entry_key).await? {
                continue;
            }
        }
        failures.push(reply);
    }
    Ok(UntrackOutcome { failures, attempts })
}

fn tally<'a>(replies: impl IntoIterator<Item = &'a Reply>) -> BTreeMap<String, usize> {
    let mut codes = BTreeMap::new();
    for reply in replies {
        let detail = reply.body["detail"].as_str().unwrap_or_default();
        *codes
            .entry(format!("{} {detail}", reply.code).trim().to_owned())
            .or_insert(0) += 1;
    }
    codes
}

fn is_ingress_refusal(reply: &Reply) -> bool {
    reply.code == ReplyCode::Overloaded.as_str() && reply.body["detail"] == INGRESS_QUEUE_FULL
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_writer_confirms_a_hand_off_to_a_least_privilege_runtime() -> TestResult {
    let server = restricted_server_or_skip!();
    let handle = start_owning_all(&server).await?;
    let client = server.client().await;
    let key: PresenceKey = "solo".parse()?;
    let topic: Topic = LOBBY.parse()?;
    let reply = send(
        &client,
        WriteOp::Track.subject(&key, &topic),
        &key,
        json!({ "holder": HolderId::generate()?, "meta": {} }),
    )
    .await?;
    assert_eq!(reply.code, "ok", "{}", reply.body);
    let confirmed = tokio::time::timeout(HANDOFF_WITHIN, async {
        while handle.stats().forwarded() == 0 {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    let stats = handle.stats();
    handle.shutdown().await;
    assert!(
        confirmed.is_ok(),
        "the ingress never saw the writer confirm the hand-off: forwarded {} of {} admitted",
        stats.forwarded(),
        stats.admitted()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lobby_then_room_burst_is_never_refused_at_ingress() -> TestResult {
    let server = restricted_server_or_skip!();
    let handle = start_owning_all(&server).await?;
    let client = server.client().await;
    let presence = service_config()?.presence().clone();

    let tracked = join_all((0..WRITERS).map(|writer| track(&client, &presence, writer))).await;
    let mut per_writer_entries = Vec::new();
    let mut track_failures = Vec::new();
    let mut all_attempts = Vec::new();
    for outcome in tracked {
        let outcome = outcome?;
        per_writer_entries.push(outcome.entries);
        track_failures.extend(outcome.failures);
        all_attempts.extend(outcome.attempts);
    }

    let untracked = join_all(per_writer_entries.iter().map(|mine| untrack(&client, &presence, mine))).await;
    let mut untrack_failures = Vec::new();
    for outcome in untracked {
        let outcome = outcome?;
        untrack_failures.extend(outcome.failures);
        all_attempts.extend(outcome.attempts);
    }

    let stats = handle.stats();
    handle.shutdown().await;

    let ingress_refusals: Vec<&Reply> = all_attempts.iter().filter(|reply| is_ingress_refusal(reply)).collect();
    assert!(
        ingress_refusals.is_empty(),
        "ingress queue refused: {:?} (admitted {}, overloaded {})",
        tally(ingress_refusals),
        stats.admitted(),
        stats.overloaded()
    );
    assert!(
        track_failures.is_empty(),
        "tracks never settled: {:?}",
        tally(&track_failures)
    );
    assert!(
        untrack_failures.is_empty(),
        "untracks never settled: {:?}",
        tally(&untrack_failures)
    );
    Ok(())
}
