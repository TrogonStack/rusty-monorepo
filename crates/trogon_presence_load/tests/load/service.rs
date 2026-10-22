//! Service profile: writers tracking on a skewed set of topics with heartbeats at the local
//! profile, readers following diffs, snapshot timing for the largest topic and process RSS.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use trogon_presence::{
    HeartbeatInterval, HolderId, LeaseTtl, PresenceConfig, PresenceKey, Presences, Topic, ViewCursor, WriterMode,
};
use trogon_presence_service::{
    PresenceReader, ReaderEvent, ReaderIdentity, ReaderOptions, ServiceConfig, ServiceHandle, WriteOp,
};

use crate::common::{BoxError, Fixture, FixtureOptions, DEFAULT_SESSION, TENANT_A};
use crate::recorder::Capture;
use crate::rss::{Rss, RssSeries};
use crate::stats::{Samples, Table, Tally};
use crate::support::{self, Browser, Entry, Reply};

const WRITERS: usize = 200;
const ROOMS: usize = 7;
const READERS: usize = 16;
const LOBBY_READERS: usize = 4;
const CYCLES: u32 = 3;
const RAMP: Duration = Duration::from_secs(5);
const SNAPSHOT_PROBES: usize = 5;
const SETUP_CONCURRENCY: usize = 16;
const CONVERGE: Duration = Duration::from_secs(30);
const READER_WARMUP: Duration = Duration::from_secs(1);
const SETTLE: Duration = Duration::from_secs(5);
const RSS_EVERY: Duration = Duration::from_millis(250);
const LOBBY: &str = "load.lobby";
const STAMP: &str = "t_us";
const FRESH_SNAPSHOT: Duration = Duration::from_secs(5);
const TTL_SLACK: Duration = Duration::from_secs(5);
const REPLY_BODIES: usize = 3;
const RETRY_ATTEMPTS: u32 = 5;
const RETRY_BASE: Duration = Duration::from_millis(50);
const RETRY_CAP: Duration = Duration::from_secs(1);
const RESIDUAL_SETTLE: Duration = Duration::from_secs(2);
const RESIDUAL_SAMPLES: usize = 3;

static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();

fn stamp() -> u64 {
    let epoch = EPOCH.get_or_init(std::time::Instant::now);
    u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn lobby() -> Result<Topic, BoxError> {
    Ok(LOBBY.parse()?)
}

fn room(index: usize) -> Result<Topic, BoxError> {
    Ok(format!("load.room{}", index % ROOMS + 1).parse()?)
}

fn writer_topics(index: usize) -> Result<Vec<Topic>, BoxError> {
    Ok(vec![lobby()?, room(index)?])
}

fn reader_topic(index: usize) -> Result<Topic, BoxError> {
    if index < LOBBY_READERS {
        lobby()
    } else {
        room(index - LOBBY_READERS)
    }
}

fn expected_entries(topic: &Topic) -> Result<usize, BoxError> {
    let mut count = 0;
    for index in 0..WRITERS {
        count += writer_topics(index)?
            .iter()
            .filter(|candidate| *candidate == topic)
            .count();
    }
    Ok(count)
}

fn entries(state: &Presences) -> usize {
    state.iter().map(|(_, metas)| metas.len()).sum()
}

fn service_config() -> Result<ServiceConfig, BoxError> {
    let presence = PresenceConfig::default().with_writer_mode(WriterMode::Managed);
    Ok(ServiceConfig::new(presence, "load-edge".parse()?))
}

/// The outcome of one command after retrying replies the service marked retryable, the way a
/// client would: bounded attempts with full-jitter exponential backoff.
struct Attempted {
    reply: Option<Reply>,
    first_code: String,
    first_body: Value,
    first_retryable: bool,
    retries: u32,
}

impl Attempted {
    fn code(&self) -> String {
        self.reply
            .as_ref()
            .map_or_else(|| "no_reply".to_owned(), |reply| reply.code.clone())
    }

    fn body(&self) -> Value {
        self.reply.as_ref().map_or(Value::Null, |reply| reply.body.clone())
    }
}

fn retryable(reply: &Reply) -> bool {
    reply.body["retryable"].as_bool() == Some(true)
}

fn backoff(retry: u32) -> Duration {
    let ceiling = RETRY_BASE.saturating_mul(1 << retry.min(16)).min(RETRY_CAP);
    let random = RandomState::new().build_hasher().finish();
    let micros = u64::try_from(ceiling.as_micros()).unwrap_or(u64::MAX).max(1);
    Duration::from_micros(random % micros)
}

async fn attempt<F, Fut>(send: F) -> Attempted
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Reply, BoxError>>,
{
    let mut first: Option<(String, Value, bool)> = None;
    let mut retries = 0;
    loop {
        let reply = send().await.ok();
        let again = reply.as_ref().is_some_and(retryable);
        if first.is_none() {
            let code = reply
                .as_ref()
                .map_or_else(|| "no_reply".to_owned(), |reply| reply.code.clone());
            let body = reply.as_ref().map_or(Value::Null, |reply| reply.body.clone());
            first = Some((code, body, again));
        }
        if !again || retries + 1 >= RETRY_ATTEMPTS {
            let (first_code, first_body, first_retryable) = first.unwrap_or_default();
            return Attempted {
                reply,
                first_code,
                first_body,
                first_retryable,
                retries,
            };
        }
        tokio::time::sleep(backoff(retries)).await;
        retries += 1;
    }
}

#[derive(Default)]
struct OpLog {
    latency: Samples,
    codes: Tally,
    first_attempt_codes: Tally,
    first_attempt_rejections: u64,
    retries: u64,
    final_failures: u64,
}

impl OpLog {
    fn record(&mut self, elapsed: Duration, attempted: &Attempted) {
        let code = attempted.code();
        self.latency.push(elapsed);
        if code != "ok" {
            self.final_failures += 1;
        }
        self.codes.bump(code);
        self.first_attempt_codes.bump(attempted.first_code.clone());
        if attempted.first_retryable {
            self.first_attempt_rejections += 1;
        }
        self.retries += u64::from(attempted.retries);
    }

    fn merge(&mut self, other: OpLog) {
        self.latency.merge(other.latency);
        self.codes.merge(other.codes);
        self.first_attempt_codes.merge(other.first_attempt_codes);
        self.first_attempt_rejections += other.first_attempt_rejections;
        self.retries += other.retries;
        self.final_failures += other.final_failures;
    }

    fn to_json(&self) -> Value {
        json!({
            "latency": self.latency.summary().to_json(),
            "codes": self.codes.to_json(),
            "first_attempt_codes": self.first_attempt_codes.to_json(),
            "first_attempt_retryable_rejections": self.first_attempt_rejections,
            "retries": self.retries,
            "final_failures": self.final_failures,
        })
    }

    fn row(&self, op: &str) -> Vec<String> {
        let mut row = vec![op.to_owned()];
        row.extend(self.latency.summary().cells());
        row.push(self.codes.render());
        row.push(self.first_attempt_rejections.to_string());
        row.push(self.retries.to_string());
        row.push(self.final_failures.to_string());
        row
    }
}

fn entry_of(holder: HolderId, topic: &Topic, body: &Value) -> Entry {
    Entry {
        holder,
        topic: topic.clone(),
        lifetime: body["lifetime"].clone(),
        mutation_seq: body["mutation_seq"].clone(),
    }
}

async fn track_all(browser: Arc<Browser>, topics: Vec<Topic>, at: Instant) -> Result<(Vec<Entry>, OpLog), BoxError> {
    tokio::time::sleep_until(at).await;
    let mut log = OpLog::default();
    let mut tracked = Vec::new();
    for topic in topics {
        let holder = HolderId::generate()?;
        let begun = Instant::now();
        let attempted = attempt(|| browser.track_raw(&topic, holder, json!({ STAMP: stamp() }))).await;
        log.record(begun.elapsed(), &attempted);
        if let Some(reply) = attempted.reply.filter(|reply| reply.code == "ok") {
            tracked.push(entry_of(holder, &topic, &reply.body));
        }
    }
    Ok((tracked, log))
}

struct Steady {
    entries: Vec<Entry>,
    heartbeat: OpLog,
    update: OpLog,
}

async fn steady(
    browser: Arc<Browser>,
    mut tracked: Vec<Entry>,
    start: Instant,
    offset: Duration,
    interval: HeartbeatInterval,
) -> Steady {
    let mut heartbeat = OpLog::default();
    let mut update = OpLog::default();
    for cycle in 0..CYCLES {
        tokio::time::sleep_until(start + offset + interval.get() * cycle).await;
        let refs: Vec<&Entry> = tracked.iter().collect();
        let begun = Instant::now();
        let attempted = attempt(|| browser.heartbeat(&refs)).await;
        heartbeat.record(begun.elapsed(), &attempted);
        if cycle != 0 {
            continue;
        }
        tokio::time::sleep_until(start + offset + interval.get() / 2).await;
        let Some(first) = tracked.first().cloned() else {
            continue;
        };
        let body = json!({
            "holder": first.holder,
            "lifetime": first.lifetime,
            "mutation_seq": first.mutation_seq,
            "meta": { STAMP: stamp(), "status": "away" },
        });
        let begun = Instant::now();
        let attempted = attempt(|| browser.command(WriteOp::Update.subject(browser.key(), &first.topic), &body)).await;
        update.record(begun.elapsed(), &attempted);
        if let Some(reply) = attempted.reply.filter(|reply| reply.code == "ok") {
            tracked[0] = entry_of(first.holder, &first.topic, &reply.body);
        }
    }
    Steady {
        entries: tracked,
        heartbeat,
        update,
    }
}

struct Untracked {
    key: String,
    topic: String,
    holder: String,
    first_code: String,
    first_body: Value,
    retries: u32,
    code: String,
    body: Value,
}

impl Untracked {
    fn to_json(&self) -> Value {
        json!({
            "holder": self.holder,
            "first_attempt_code": self.first_code,
            "retries": self.retries,
            "code": self.code,
            "body": self.body,
        })
    }
}

async fn untrack_all(browser: Arc<Browser>, tracked: Vec<Entry>) -> (OpLog, Vec<Untracked>) {
    let mut log = OpLog::default();
    let mut outcomes = Vec::new();
    for entry in tracked {
        let begun = Instant::now();
        let attempted = attempt(|| browser.untrack(&entry)).await;
        log.record(begun.elapsed(), &attempted);
        outcomes.push(Untracked {
            key: browser.key().to_string(),
            topic: entry.topic.as_str().to_owned(),
            holder: serde_json::to_value(entry.holder)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            first_code: attempted.first_code.clone(),
            first_body: attempted.first_body.clone(),
            retries: attempted.retries,
            code: attempted.code(),
            body: attempted.body(),
        });
    }
    (log, outcomes)
}

type LastCursor = Arc<Mutex<Option<ViewCursor>>>;

fn cursor_json(cursor: Option<ViewCursor>) -> Value {
    cursor.map_or(Value::Null, |cursor| json!(format!("{cursor:?}")))
}

#[derive(Default)]
struct Following {
    lag: Samples,
    diffs: u64,
    snapshots: u64,
    lagged_events: u64,
}

async fn follow(
    mut events: broadcast::Receiver<ReaderEvent>,
    mut stop: watch::Receiver<bool>,
    last: LastCursor,
) -> Following {
    let mut seen = Following::default();
    loop {
        tokio::select! {
            _ = stop.changed() => return seen,
            event = events.recv() => match event {
                Ok(ReaderEvent::Diff(applied)) => {
                    let now = stamp();
                    seen.diffs += 1;
                    *last.lock().unwrap_or_else(PoisonError::into_inner) = Some(applied.cursor());
                    for (_, metas) in applied.diff().joins().iter() {
                        for entry in metas {
                            if let Some(sent) = entry.meta().as_map().get(STAMP).and_then(Value::as_u64) {
                                seen.lag.push(Duration::from_micros(now.saturating_sub(sent)));
                            }
                        }
                    }
                }
                Ok(ReaderEvent::Snapshot { cursor, .. }) => {
                    seen.snapshots += 1;
                    *last.lock().unwrap_or_else(PoisonError::into_inner) = Some(cursor);
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => seen.lagged_events += skipped,
                Err(broadcast::error::RecvError::Closed) => return seen,
            },
        }
    }
}

/// Waits for a reader to hold `expected` entries and returns what it holds when it stops waiting.
async fn converged(reader: &PresenceReader, expected: usize) -> usize {
    let mut state = reader.watch();
    let waited = tokio::time::timeout(CONVERGE, async {
        loop {
            let held = entries(&state.borrow_and_update());
            if held == expected {
                return held;
            }
            if state.changed().await.is_err() {
                return held;
            }
        }
    })
    .await;
    waited.unwrap_or_else(|_| entries(&reader.presences()))
}

#[derive(Default)]
struct Convergence {
    elapsed: Duration,
    mismatched: Vec<Value>,
}

impl Convergence {
    async fn of(
        readers: &[(PresenceReader, Topic)],
        target: impl Fn(&Topic) -> Result<usize, BoxError>,
    ) -> Result<Self, BoxError> {
        let started = Instant::now();
        let mut waits = Vec::new();
        for (reader, topic) in readers {
            let expected = target(topic)?;
            waits.push(async move { (topic, expected, converged(reader, expected).await) });
        }
        let mismatched = futures_util::future::join_all(waits)
            .await
            .into_iter()
            .filter(|(_, expected, held)| held != expected)
            .map(|(topic, expected, held)| json!({ "topic": topic.as_str(), "expected": expected, "held": held }))
            .collect();
        Ok(Self {
            elapsed: started.elapsed(),
            mismatched,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "elapsed_ms": crate::stats::round(self.elapsed.as_secs_f64() * 1000.0),
            "mismatched_readers": self.mismatched,
        })
    }

    fn render(&self) -> String {
        if self.mismatched.is_empty() {
            "all converged".to_owned()
        } else {
            format!("{} readers off", self.mismatched.len())
        }
    }
}

async fn read(browser: &Browser, topic: Topic) -> Result<PresenceReader, BoxError> {
    let identity = ReaderIdentity::new(browser.key().clone(), browser.identity.cid);
    Ok(PresenceReader::start(browser.client().clone(), identity, topic, ReaderOptions::default()).await?)
}

async fn snapshot_probe(browser: &Browser, expected: usize) -> Result<Duration, BoxError> {
    let begun = Instant::now();
    let reader = read(browser, lobby()?).await?;
    let held = converged(&reader, expected).await;
    let elapsed = begun.elapsed();
    reader.close().await;
    if held == expected {
        Ok(elapsed)
    } else {
        Err(format!("snapshot probe held {held} of {expected} entries").into())
    }
}

fn detail_of(code: &str, body: &Value) -> String {
    let detail = body
        .get("detail")
        .or_else(|| body.get("message"))
        .map_or_else(|| body.to_string(), Value::to_string);
    format!("{code}: {detail}")
}

/// First-attempt rejections of untrack by reply detail and topic, and what remained after retries.
fn untrack_failures(untracked: &[Untracked]) -> Value {
    let rejected: Vec<&Untracked> = untracked.iter().filter(|outcome| outcome.first_code != "ok").collect();
    let mut first_by_detail = Tally::default();
    let mut first_by_topic = Tally::default();
    for outcome in &rejected {
        first_by_detail.bump(detail_of(&outcome.first_code, &outcome.first_body));
        first_by_topic.bump(outcome.topic.clone());
    }
    let mut final_by_detail = Tally::default();
    for outcome in untracked.iter().filter(|outcome| outcome.code != "ok") {
        final_by_detail.bump(detail_of(&outcome.code, &outcome.body));
    }
    let mut retries_per_entry = Tally::default();
    for outcome in untracked {
        retries_per_entry.bump(outcome.retries.to_string());
    }
    json!({
        "first_attempt_rejections": rejected.len(),
        "first_attempt_by_detail": first_by_detail.to_json(),
        "first_attempt_by_topic": first_by_topic.to_json(),
        "retries_per_entry": retries_per_entry.to_json(),
        "final_by_detail": final_by_detail.to_json(),
        "sample_first_attempt_bodies": rejected.iter().take(REPLY_BODIES).map(|outcome| outcome.first_body.clone()).collect::<Vec<_>>(),
    })
}

/// A fresh reader's first snapshot of `topic`: its cursor and state.
async fn fresh_view(browser: &Browser, topic: &Topic) -> Result<(Option<ViewCursor>, Presences), BoxError> {
    let reader = read(browser, topic.clone()).await?;
    let mut events = reader.events();
    let cursor = tokio::time::timeout(FRESH_SNAPSHOT, async {
        loop {
            match events.recv().await {
                Ok(ReaderEvent::Snapshot { cursor, .. }) => return Some(cursor),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();
    let state = reader.presences();
    reader.close().await;
    Ok((cursor, state))
}

/// Entries readers still hold shortly after every untrack returned, captured before TTL expiry can
/// remove them, so each one can be tied to its untrack reply and checked against the service.
struct Residual {
    records: Vec<(usize, PresenceKey, Value)>,
    captured: Instant,
}

impl Residual {
    async fn capture(
        readers: &[(PresenceReader, Topic)],
        cursors: &[LastCursor],
        probe: &Browser,
        untracked: &[Untracked],
    ) -> Result<Self, BoxError> {
        tokio::time::sleep(RESIDUAL_SETTLE).await;
        let captured = Instant::now();
        let mut records = Vec::new();
        for (index, (reader, topic)) in readers.iter().enumerate() {
            for (key, metas) in reader.presences().iter() {
                let replies: Vec<Value> = untracked
                    .iter()
                    .filter(|outcome| outcome.key == key.to_string() && outcome.topic == topic.as_str())
                    .map(Untracked::to_json)
                    .collect();
                records.push((
                    index,
                    key.clone(),
                    json!({
                        "reader": index,
                        "topic": topic.as_str(),
                        "key": key.to_string(),
                        "metas": serde_json::to_value(metas)?,
                        "untrack_replies": replies,
                    }),
                ));
            }
        }
        let mut residual = Self { records, captured };
        residual.check(readers, cursors, probe, "now").await?;
        Ok(residual)
    }

    async fn check(
        &mut self,
        readers: &[(PresenceReader, Topic)],
        cursors: &[LastCursor],
        probe: &Browser,
        phase: &str,
    ) -> Result<(), BoxError> {
        let mut views: HashMap<String, (Option<ViewCursor>, Presences)> = HashMap::new();
        for (index, key, record) in &mut self.records {
            let (reader, topic) = &readers[*index];
            if !views.contains_key(topic.as_str()) {
                views.insert(topic.as_str().to_owned(), fresh_view(probe, topic).await?);
            }
            let last = cursors
                .get(*index)
                .and_then(|last| *last.lock().unwrap_or_else(PoisonError::into_inner));
            if let Some((cursor, state)) = views.get(topic.as_str()) {
                record[phase] = json!({
                    "since_capture_s": crate::stats::round(self.captured.elapsed().as_secs_f64()),
                    "reader_holds_key": reader.presences().get(key).is_some(),
                    "fresh_snapshot_lists_key": state.get(key).is_some(),
                    "fresh_snapshot_entries": entries(state),
                    "fresh_cursor": cursor_json(*cursor),
                    "reader_cursor": cursor_json(last),
                });
            }
        }
        Ok(())
    }

    /// Waits until one presence TTL plus slack has passed since capture, then checks again.
    async fn after_ttl(
        mut self,
        readers: &[(PresenceReader, Topic)],
        cursors: &[LastCursor],
        probe: &Browser,
        presence_ttl: LeaseTtl,
    ) -> Result<Value, BoxError> {
        if !self.records.is_empty() {
            let deadline = presence_ttl.get() + TTL_SLACK;
            tokio::time::sleep(deadline.saturating_sub(self.captured.elapsed())).await;
            self.check(readers, cursors, probe, "after_one_ttl").await?;
        }
        Ok(self.summary())
    }

    fn summary(self) -> Value {
        let flag = |record: &Value, phase: &str, field: &str| record[phase][field].as_bool() == Some(true);
        let mut by_reply = Tally::default();
        let mut held_keys = std::collections::BTreeSet::new();
        for (_, key, record) in &self.records {
            held_keys.insert(key.to_string());
            let codes: Vec<String> = record["untrack_replies"]
                .as_array()
                .map(|replies| {
                    replies
                        .iter()
                        .filter_map(|reply| reply["code"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            by_reply.bump(if codes.is_empty() {
                "no untrack sent".to_owned()
            } else {
                codes.join("+")
            });
        }
        let count = |phase: &str, field: &str| {
            self.records
                .iter()
                .filter(|(_, _, record)| flag(record, phase, field))
                .count()
        };
        json!({
            "settle_after_untrack_s": RESIDUAL_SETTLE.as_secs(),
            "reader_entries": self.records.len(),
            "distinct_keys": held_keys.len(),
            "untrack_reply_for_residual": by_reply.to_json(),
            "fresh_snapshot_lists_key_now": count("now", "fresh_snapshot_lists_key"),
            "reader_holds_key_after_one_ttl": count("after_one_ttl", "reader_holds_key"),
            "fresh_snapshot_lists_key_after_one_ttl": count("after_one_ttl", "fresh_snapshot_lists_key"),
            "samples": self.records.into_iter().take(RESIDUAL_SAMPLES).map(|(_, _, record)| record).collect::<Vec<_>>(),
        })
    }
}

async fn join_all(fixture: &Fixture, prefix: &str, plan: Vec<Vec<Topic>>) -> Result<Vec<Arc<Browser>>, BoxError> {
    let joined: Vec<(usize, Result<Browser, BoxError>)> = stream::iter(plan.into_iter().enumerate())
        .map(|(index, granted)| async move {
            let sub = format!("{prefix}{index:04}");
            (
                index,
                Browser::join(fixture, TENANT_A, &sub, "s1", &granted, DEFAULT_SESSION).await,
            )
        })
        .buffer_unordered(SETUP_CONCURRENCY)
        .collect()
        .await;
    let mut ordered: Vec<(usize, Browser)> = Vec::new();
    for (index, browser) in joined {
        ordered.push((index, browser?));
    }
    ordered.sort_by_key(|(index, _)| *index);
    Ok(ordered.into_iter().map(|(_, browser)| Arc::new(browser)).collect())
}

fn sample_rss(stop: watch::Receiver<bool>) -> JoinHandle<RssSeries> {
    tokio::spawn(async move {
        let mut series = RssSeries::default();
        let mut tick = tokio::time::interval(RSS_EVERY);
        loop {
            tick.tick().await;
            if let Some(rss) = Rss::sample() {
                series.push(rss);
            }
            if *stop.borrow() {
                return series;
            }
        }
    })
}

fn rss_point() -> Value {
    Rss::sample().map_or(Value::Null, Rss::to_json)
}

pub struct ServiceReport {
    pub json: Value,
    pub tables: Vec<Table>,
}

pub async fn measure(capture: &Arc<Capture>) -> Result<Option<ServiceReport>, BoxError> {
    let options = FixtureOptions {
        websocket: true,
        ..FixtureOptions::default()
    };
    let Some(fixture) = Fixture::start(options).await else {
        return Ok(None);
    };
    let config = service_config()?;
    let interval = config.presence().heartbeat();
    let presence_ttl = config.presence().lease_ttl();
    let before_service = rss_point();
    let service = support::provision_and_start(&fixture, &fixture.users.app_a, config).await?;
    let outcome = profile(&fixture, &service, interval, presence_ttl, before_service).await;
    let stats = service.stats();
    service.shutdown().await;
    capture.take();
    let (mut json, tables) = outcome?;
    json["service_admission"] = json!({
        "forwarded": stats.forwarded(),
        "admitted": stats.admitted(),
        "rejected_inbox": stats.rejected_inbox(),
        "rate_limited": stats.rate_limited(),
        "overloaded": stats.overloaded(),
    });
    Ok(Some(ServiceReport { json, tables }))
}

async fn profile(
    fixture: &Fixture,
    service: &ServiceHandle,
    interval: HeartbeatInterval,
    presence_ttl: LeaseTtl,
    before_service: Value,
) -> Result<(Value, Vec<Table>), BoxError> {
    let writers = join_all(
        fixture,
        "writer",
        (0..WRITERS).map(writer_topics).collect::<Result<_, _>>()?,
    )
    .await?;
    let reader_plan: Vec<Topic> = (0..READERS).map(reader_topic).collect::<Result<_, _>>()?;
    let reader_browsers = join_all(
        fixture,
        "reader",
        reader_plan.iter().map(|topic| vec![topic.clone()]).collect(),
    )
    .await?;
    let every_topic: Vec<Topic> = std::iter::once(lobby())
        .chain((0..ROOMS).map(room))
        .collect::<Result<_, _>>()?;
    let probe = join_all(fixture, "probe", vec![every_topic]).await?;
    let probe = probe.first().ok_or("no snapshot probe browser")?.clone();

    let (stop_follow, stop_follow_rx) = watch::channel(false);
    let mut readers = Vec::new();
    let mut followers = Vec::new();
    let mut cursors: Vec<LastCursor> = Vec::new();
    for (browser, topic) in reader_browsers.iter().zip(&reader_plan) {
        let reader = read(browser, topic.clone()).await?;
        let last = LastCursor::default();
        cursors.push(last.clone());
        followers.push(tokio::spawn(follow(reader.events(), stop_follow_rx.clone(), last)));
        readers.push((reader, topic.clone()));
    }
    tokio::time::sleep(READER_WARMUP).await;
    let idle = rss_point();

    let (stop_rss, stop_rss_rx) = watch::channel(false);
    let sampler = sample_rss(stop_rss_rx);
    let load_started = Instant::now();
    let ramp_start = Instant::now();
    let spacing = RAMP / u32::try_from(WRITERS)?;
    let tracking: Vec<_> = writers
        .iter()
        .enumerate()
        .map(|(index, browser)| {
            let at = ramp_start + spacing * u32::try_from(index).unwrap_or(u32::MAX);
            let topics = writer_topics(index);
            let browser = browser.clone();
            tokio::spawn(async move { track_all(browser, topics?, at).await })
        })
        .collect();
    let mut track = OpLog::default();
    let mut tracked = Vec::new();
    for task in tracking {
        let (entries, log) = task.await??;
        track.merge(log);
        tracked.push(entries);
    }
    let after_ramp = Convergence::of(&readers, expected_entries).await?;

    let largest = expected_entries(&lobby()?)?;
    let mut snapshot = Samples::default();
    let mut snapshot_failures = Tally::default();
    for _ in 0..SNAPSHOT_PROBES {
        match snapshot_probe(&probe, largest).await {
            Ok(elapsed) => snapshot.push(elapsed),
            Err(err) => snapshot_failures.bump(err.to_string()),
        }
    }

    let steady_start = Instant::now();
    let beat_spacing = interval.get() / u32::try_from(WRITERS)?;
    let beating: Vec<_> = writers
        .iter()
        .zip(tracked)
        .enumerate()
        .map(|(index, (browser, entries))| {
            let offset = beat_spacing * u32::try_from(index).unwrap_or(u32::MAX);
            tokio::spawn(steady(browser.clone(), entries, steady_start, offset, interval))
        })
        .collect();
    let mut heartbeat = OpLog::default();
    let mut update = OpLog::default();
    let mut remaining = Vec::new();
    for task in beating {
        let done = task.await?;
        heartbeat.merge(done.heartbeat);
        update.merge(done.update);
        remaining.push(done.entries);
    }
    let after_steady = Convergence::of(&readers, expected_entries).await?;

    let untracking: Vec<_> = writers
        .iter()
        .zip(remaining)
        .map(|(browser, entries)| tokio::spawn(untrack_all(browser.clone(), entries)))
        .collect();
    let mut untrack = OpLog::default();
    let mut untracked = Vec::new();
    for task in untracking {
        let (log, outcomes) = task.await?;
        untrack.merge(log);
        untracked.extend(outcomes);
    }
    let residual = Residual::capture(&readers, &cursors, &probe, &untracked).await?;
    let after_untrack = Convergence::of(&readers, |_| Ok(0)).await?;
    let load_wall = load_started.elapsed();
    let _ = stop_rss.send(true);
    let during = sampler.await?;
    tokio::time::sleep(SETTLE).await;
    let after = rss_point();
    let residual = residual.after_ttl(&readers, &cursors, &probe, presence_ttl).await?;

    let _ = stop_follow.send(true);
    let mut following = Following::default();
    for task in followers {
        let seen = task.await?;
        following.lag.merge(seen.lag);
        following.diffs += seen.diffs;
        following.snapshots += seen.snapshots;
        following.lagged_events += seen.lagged_events;
    }
    for (reader, _) in readers {
        reader.close().await;
    }
    let owned = service.owned_shards().len();

    let mut ops = Table::new(
        "service commands (latency in ms)",
        &[
            "op",
            "count",
            "p50",
            "p95",
            "p99",
            "max",
            "final codes",
            "first-attempt retryable",
            "retries",
            "final failures",
        ],
    );
    ops.row(track.row("track"));
    ops.row(heartbeat.row("heartbeat"));
    ops.row(update.row("update"));
    ops.row(untrack.row("untrack"));
    let mut lag_row = vec!["diff lag (join and update)".to_owned()];
    lag_row.extend(following.lag.summary().cells());
    lag_row.push(format!(
        "diffs={} lagged_events={}",
        following.diffs, following.lagged_events
    ));
    ops.row(lag_row);
    let mut snapshot_row = vec![format!("snapshot {largest} entries")];
    snapshot_row.extend(snapshot.summary().cells());
    snapshot_row.push(snapshot_failures.render());
    ops.row(snapshot_row);
    let mut readers_table = Table::new(
        format!(
            "reader convergence ({READERS} readers, {} s timeout)",
            CONVERGE.as_secs()
        ),
        &["after ramp", "after steady", "after untrack"],
    );
    readers_table.row(vec![
        format!(
            "{} in {:.0} ms",
            after_ramp.render(),
            after_ramp.elapsed.as_secs_f64() * 1000.0
        ),
        format!(
            "{} in {:.0} ms",
            after_steady.render(),
            after_steady.elapsed.as_secs_f64() * 1000.0
        ),
        format!(
            "{} in {:.0} ms",
            after_untrack.render(),
            after_untrack.elapsed.as_secs_f64() * 1000.0
        ),
    ]);

    let mut rss = Table::new(
        format!("process RSS in MiB ({})", crate::rss::SOURCE),
        &["before service", "idle", "load peak", "load mean", "after load"],
    );
    let during_json = during.to_json();
    let show = |value: &Value| value.as_f64().map_or_else(|| "-".to_owned(), |mib| format!("{mib:.1}"));
    rss.row(vec![
        show(&before_service),
        show(&idle),
        show(&during_json["peak_mib"]),
        show(&during_json["mean_mib"]),
        show(&after),
    ]);

    let json = json!({
        "parameters": {
            "writers": WRITERS,
            "topics": ROOMS + 1,
            "entries_per_writer": 2,
            "largest_topic_entries": largest,
            "readers": READERS,
            "lobby_readers": LOBBY_READERS,
            "heartbeat_interval_s": interval.get().as_secs(),
            "presence_ttl_s": presence_ttl.get().as_secs(),
            "heartbeat_cycles": CYCLES,
            "ramp_s": RAMP.as_secs(),
            "snapshot_probes": SNAPSHOT_PROBES,
            "transport": "websocket",
            "writer_mode": "managed",
            "owned_shards": owned,
        },
        "load_wall_s": crate::stats::round(load_wall.as_secs_f64()),
        "converge_after_ramp": after_ramp.to_json(),
        "converge_after_steady": after_steady.to_json(),
        "converge_after_untrack": after_untrack.to_json(),
        "untrack_failures": untrack_failures(&untracked),
        "residual_entries": residual,
        "snapshot_failures": snapshot_failures.to_json(),
        "track": track.to_json(),
        "heartbeat": heartbeat.to_json(),
        "update": update.to_json(),
        "untrack": untrack.to_json(),
        "diff_lag": following.lag.summary().to_json(),
        "diffs_seen": following.diffs,
        "reader_snapshots": following.snapshots,
        "lagged_events": following.lagged_events,
        "snapshot_largest_topic": snapshot.summary().to_json(),
        "rss": {
            "source": crate::rss::SOURCE,
            "scope": "whole test process: service, auth callout, readers and every client connection",
            "before_service_mib": before_service,
            "idle_mib": idle,
            "during_load": during_json,
            "after_load_mib": after,
        },
    });
    Ok((json, vec![ops, readers_table, rss]))
}
