mod common;

use std::error::Error;
use std::time::Duration;

use async_nats::{HeaderMap, Subscriber};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use trogon_presence::watch::ViewCursor;
use trogon_presence::{
    ConnectionId, Diff, DiffSequence, EntryRevision, GenerationEpoch, OwnerEpoch, OwnerId, PresenceKey, Presences,
    RequestId, ShardCount, SnapshotId, StreamGeneration, Topic,
};
use trogon_presence_service::reply::{epoch_headers, HEADER_CODE, HEADER_KIND, HEADER_PREV, HEADER_SEQ};
use trogon_presence_service::snapshot::{
    AssembliesPerConnection, AssemblyDeadline, CapturedSnapshot, PartIndex, SnapshotFrame, SnapshotIdentity,
    SnapshotManifest,
};
use trogon_presence_service::subjects::{diff_subject, SnapshotReplySubject};
use trogon_presence_service::{
    AssemblyGate, PayloadBudget, PresenceReader, ReaderEvent, ReaderIdentity, ReaderOptions, ResnapshotInterval,
    SnapshotLimits,
};

use common::NatsServer;

type BoxError = Box<dyn Error + Send + Sync>;
type TestResult = Result<(), BoxError>;

const TOPIC: &str = "room:lobby";
const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(600);
const ANA_REF: &str = "AQEBAQEBAQEBAQEBAQEBAQ";
const BOB_REF: &str = "AgICAgICAgICAgICAgICAg";
const CAROL_REF: &str = "AwMDAwMDAwMDAwMDAwMDAw";

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

fn epoch(generation: u8, acquired: u64) -> GenerationEpoch {
    GenerationEpoch::new(
        StreamGeneration::from([generation; 16]),
        OwnerEpoch::new(EntryRevision::from(acquired), OwnerId::from([7; 16])),
    )
}

fn state(entries: &[(&str, &str)]) -> Result<Presences, BoxError> {
    let mut map = serde_json::Map::new();
    for (key, phx_ref) in entries {
        map.insert(
            (*key).to_owned(),
            json!({ "metas": [{ "phx_ref": phx_ref, "status": "online" }] }),
        );
    }
    Ok(serde_json::from_value(Value::Object(map))?)
}

fn joining(key: &str, phx_ref: &str) -> Result<Diff, BoxError> {
    Ok(serde_json::from_value(json!({
        "joins": { key: { "metas": [{ "phx_ref": phx_ref, "status": "online" }] } },
        "leaves": {},
    }))?)
}

fn keys(presences: &Presences) -> Vec<String> {
    presences.iter().map(|(key, _)| key.to_string()).collect()
}

struct Request {
    id: RequestId,
    inbox: String,
}

struct FakeRuntime {
    client: async_nats::Client,
    caller: PresenceKey,
    connection: ConnectionId,
    topic: Topic,
    requests: Subscriber,
}

impl FakeRuntime {
    async fn start(server: &NatsServer) -> Result<Self, BoxError> {
        let client = server.client().await;
        let caller: PresenceKey = "reader".parse()?;
        let connection = ConnectionId::generate()?;
        let requests = client
            .subscribe(format!("presence.v1.snapshot.*.{}.{connection}.>", caller.token()))
            .await?;
        client.flush().await?;
        Ok(Self {
            client,
            caller,
            connection,
            topic: TOPIC.parse()?,
            requests,
        })
    }

    async fn reader(&self, options: ReaderOptions) -> Result<PresenceReader, BoxError> {
        Ok(PresenceReader::start(
            self.client.clone(),
            ReaderIdentity::new(self.caller.clone(), self.connection),
            self.topic.clone(),
            options,
        )
        .await?)
    }

    async fn next_request(&mut self, within: Duration) -> Result<Option<Request>, BoxError> {
        let Ok(message) = tokio::time::timeout(within, self.requests.next()).await else {
            return Ok(None);
        };
        let message = message.ok_or("request subscription ended")?;
        let body: Value = serde_json::from_slice(&message.payload)?;
        let id: RequestId = body["request_id"].as_str().ok_or("request without an id")?.parse()?;
        let inbox = message.reply.ok_or("request without a reply inbox")?.to_string();
        Ok(Some(Request { id, inbox }))
    }

    async fn request(&mut self) -> Result<Request, BoxError> {
        self.next_request(WAIT)
            .await?
            .ok_or_else(|| "reader sent no snapshot request".into())
    }

    fn capture(
        &self,
        request: &Request,
        at: GenerationEpoch,
        seq: u64,
        presences: &Presences,
    ) -> Result<CapturedSnapshot, BoxError> {
        self.capture_in(request, at, seq, presences, SnapshotLimits::default())
    }

    fn capture_in(
        &self,
        request: &Request,
        at: GenerationEpoch,
        seq: u64,
        presences: &Presences,
        limits: SnapshotLimits,
    ) -> Result<CapturedSnapshot, BoxError> {
        let identity = SnapshotIdentity::new(request.id, SnapshotId::generate()?, at, DiffSequence::from(seq));
        Ok(CapturedSnapshot::capture(identity, presences, limits)?)
    }

    async fn respond(&self, request: &Request, manifest: &SnapshotManifest, frames: Vec<SnapshotFrame>) -> TestResult {
        let identity = manifest.identity();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_CODE, "ok");
        headers.insert(HEADER_KIND, "manifest");
        headers.insert(HEADER_SEQ, identity.seq().get().to_string());
        epoch_headers(&mut headers, identity.epoch());
        self.client
            .publish_with_headers(request.inbox.clone(), headers, serde_json::to_vec(manifest)?.into())
            .await?;
        self.publish(frames).await
    }

    async fn publish(&self, frames: Vec<SnapshotFrame>) -> TestResult {
        for frame in frames {
            let reply = SnapshotReplySubject::new(&self.caller, &self.connection, &frame.identity().snapshot());
            frame.publish(&self.client, &reply).await?;
        }
        self.client.flush().await?;
        Ok(())
    }

    async fn serve(&mut self, at: GenerationEpoch, seq: u64, presences: &Presences) -> TestResult {
        let request = self.request().await?;
        let captured = self.capture(&request, at, seq, presences)?;
        self.respond(&request, captured.manifest(), captured.frames()).await
    }

    async fn diff(&self, at: GenerationEpoch, seq: u64, prev: u64, diff: &Diff) -> TestResult {
        let mut headers = HeaderMap::new();
        epoch_headers(&mut headers, at);
        headers.insert(HEADER_SEQ, seq.to_string());
        headers.insert(HEADER_PREV, prev.to_string());
        headers.insert(HEADER_KIND, "diff");
        self.client
            .publish_with_headers(diff_subject(&self.topic), headers, serde_json::to_vec(diff)?.into())
            .await?;
        self.client.flush().await?;
        Ok(())
    }
}

async fn next_event(events: &mut broadcast::Receiver<ReaderEvent>) -> Result<ReaderEvent, BoxError> {
    Ok(tokio::time::timeout(WAIT, events.recv()).await??)
}

async fn snapshot_event(events: &mut broadcast::Receiver<ReaderEvent>) -> Result<(ViewCursor, Presences), BoxError> {
    match next_event(events).await? {
        ReaderEvent::Snapshot { cursor, state } => Ok((cursor, state)),
        other => Err(format!("expected a snapshot event, got {other:?}").into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_state_then_replays_buffered_diffs() -> TestResult {
    let server = server_or_skip!();
    let mut runtime = FakeRuntime::start(&server).await?;
    let reader = runtime.reader(ReaderOptions::default()).await?;
    let mut events = reader.events();
    let at = epoch(1, 10);

    let request = runtime.request().await?;
    runtime.diff(at, 5, 4, &joining("ana", ANA_REF)?).await?;
    runtime.diff(at, 6, 5, &joining("bob", BOB_REF)?).await?;
    let captured = runtime.capture(&request, at, 5, &state(&[("ana", ANA_REF)])?)?;
    runtime
        .respond(&request, captured.manifest(), captured.frames())
        .await?;

    let (cursor, installed) = snapshot_event(&mut events).await?;
    assert_eq!(
        cursor,
        ViewCursor::Service {
            epoch: at,
            seq: DiffSequence::from(5)
        }
    );
    assert_eq!(keys(&installed), ["ana"]);
    match next_event(&mut events).await? {
        ReaderEvent::Diff(applied) => {
            assert_eq!(applied.cursor().seq(), DiffSequence::from(6));
            assert_eq!(applied.diff(), &joining("bob", BOB_REF)?);
        }
        other => return Err(format!("expected the buffered diff, got {other:?}").into()),
    }
    assert_eq!(keys(&reader.presences()), ["ana", "bob"]);

    runtime.diff(at, 7, 6, &joining("carol", CAROL_REF)?).await?;
    assert!(matches!(next_event(&mut events).await?, ReaderEvent::Diff(_)));
    assert_eq!(keys(&reader.presences()), ["ana", "bob", "carol"]);
    reader.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn digest_mismatch_abandons_and_the_retry_succeeds() -> TestResult {
    let server = server_or_skip!();
    let mut runtime = FakeRuntime::start(&server).await?;
    let reader = runtime.reader(ReaderOptions::default()).await?;
    let mut events = reader.events();
    let at = epoch(1, 10);
    let presences = state(&[("ana", ANA_REF)])?;

    let request = runtime.request().await?;
    let captured = runtime.capture(&request, at, 3, &presences)?;
    let tampered = captured
        .frames()
        .into_iter()
        .map(|frame| match frame {
            SnapshotFrame::Part { identity, index, bytes } => {
                let mut flipped = bytes.to_vec();
                let position = flipped
                    .windows(6)
                    .position(|window| window == b"online")
                    .unwrap_or_default();
                flipped[position] = b'O';
                SnapshotFrame::Part {
                    identity,
                    index,
                    bytes: Bytes::from(flipped),
                }
            }
            end => end,
        })
        .collect();
    runtime.respond(&request, captured.manifest(), tampered).await?;

    let retry = runtime.request().await?;
    assert_ne!(retry.id, request.id, "a retry must carry a fresh request id");
    let captured = runtime.capture(&retry, at, 3, &presences)?;
    runtime.respond(&retry, captured.manifest(), captured.frames()).await?;
    let (_, installed) = snapshot_event(&mut events).await?;
    assert_eq!(installed, presences);
    reader.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diff_from_another_generation_triggers_a_resnapshot() -> TestResult {
    let server = server_or_skip!();
    let mut runtime = FakeRuntime::start(&server).await?;
    let reader = runtime.reader(ReaderOptions::default()).await?;
    let mut events = reader.events();
    let at = epoch(1, 10);
    runtime.serve(at, 4, &state(&[("ana", ANA_REF)])?).await?;
    snapshot_event(&mut events).await?;

    let rebuilt = epoch(2, 10);
    runtime.diff(rebuilt, 5, 4, &joining("bob", BOB_REF)?).await?;
    let presences = state(&[("bob", BOB_REF)])?;
    runtime.serve(rebuilt, 9, &presences).await?;
    let (cursor, installed) = snapshot_event(&mut events).await?;
    assert_eq!(
        cursor,
        ViewCursor::Service {
            epoch: rebuilt,
            seq: DiffSequence::from(9)
        }
    );
    assert_eq!(installed, presences);
    reader.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diff_from_a_stale_owner_epoch_is_ignored() -> TestResult {
    let server = server_or_skip!();
    let mut runtime = FakeRuntime::start(&server).await?;
    let reader = runtime.reader(ReaderOptions::default()).await?;
    let mut events = reader.events();
    let at = epoch(1, 10);
    runtime.serve(at, 4, &state(&[("ana", ANA_REF)])?).await?;
    snapshot_event(&mut events).await?;

    runtime.diff(epoch(1, 5), 5, 4, &joining("bob", BOB_REF)?).await?;
    assert!(
        runtime.next_request(QUIET).await?.is_none(),
        "a stale owner epoch must not trigger a resnapshot"
    );
    assert_eq!(keys(&reader.presences()), ["ana"]);

    runtime.diff(at, 5, 4, &joining("carol", CAROL_REF)?).await?;
    match next_event(&mut events).await? {
        ReaderEvent::Diff(applied) => assert_eq!(applied.diff(), &joining("carol", CAROL_REF)?),
        other => return Err(format!("expected the current owner's diff, got {other:?}").into()),
    }
    assert_eq!(keys(&reader.presences()), ["ana", "carol"]);
    reader.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_resnapshot_timer_fires_on_the_configured_interval() -> TestResult {
    let server = server_or_skip!();
    let mut runtime = FakeRuntime::start(&server).await?;
    let interval = ResnapshotInterval::try_from(Duration::from_millis(300))?;
    let reader = runtime
        .reader(
            ReaderOptions::default()
                .with_shards(ShardCount::DEFAULT)
                .with_resnapshot(interval),
        )
        .await?;
    let mut events = reader.events();
    let at = epoch(1, 10);
    runtime.serve(at, 4, &state(&[("ana", ANA_REF)])?).await?;
    snapshot_event(&mut events).await?;

    let started = tokio::time::Instant::now();
    let presences = state(&[("ana", ANA_REF), ("bob", BOB_REF)])?;
    runtime.serve(at, 6, &presences).await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(250),
        "resnapshot came after {elapsed:?}"
    );
    let (_, installed) = snapshot_event(&mut events).await?;
    assert_eq!(installed, presences);

    assert!(ResnapshotInterval::try_from(Duration::ZERO).is_err());
    assert_eq!(ResnapshotInterval::default().get(), Duration::from_secs(30));
    reader.close().await;
    Ok(())
}

const CHUNK_DEADLINE: Duration = Duration::from_millis(400);
const DEADLINE_FLOOR: Duration = Duration::from_millis(350);
const BASELINE_RESNAPSHOT: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy)]
struct CrowdLabel(&'static str);

const CROWD: usize = 12;

fn crowd(label: CrowdLabel) -> Result<Presences, BoxError> {
    const ALPHABET: &[u8] = b"BCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut map = serde_json::Map::new();
    for (index, letter) in ALPHABET.iter().take(CROWD).enumerate() {
        let phx_ref = format!("{}A", char::from(*letter).to_string().repeat(21));
        map.insert(
            format!("member-{index:02}"),
            json!({ "metas": [{ "phx_ref": phx_ref, "status": "online", "bio": format!("{}-{}", label.0, "x".repeat(96)) }] }),
        );
    }
    Ok(serde_json::from_value(Value::Object(map))?)
}

fn chunked_limits() -> Result<SnapshotLimits, BoxError> {
    Ok(SnapshotLimits::default()
        .with_payload(PayloadBudget::try_from(1024)?)
        .with_deadline(AssemblyDeadline::try_from(CHUNK_DEADLINE)?))
}

fn parts_of(frames: &[SnapshotFrame]) -> usize {
    frames
        .iter()
        .filter(|frame| matches!(frame, SnapshotFrame::Part { .. }))
        .count()
}

fn without_part(frames: Vec<SnapshotFrame>, missing: PartIndex) -> Vec<SnapshotFrame> {
    frames
        .into_iter()
        .filter(|frame| !matches!(frame, SnapshotFrame::Part { index, .. } if *index == missing))
        .collect()
}

fn without_end(frames: Vec<SnapshotFrame>) -> Vec<SnapshotFrame> {
    frames
        .into_iter()
        .filter(|frame| !matches!(frame, SnapshotFrame::End { .. }))
        .collect()
}

fn only_part(frames: &[SnapshotFrame], wanted: PartIndex) -> Result<SnapshotFrame, BoxError> {
    frames
        .iter()
        .find(|frame| matches!(frame, SnapshotFrame::Part { index, .. } if *index == wanted))
        .cloned()
        .ok_or_else(|| format!("no part {wanted}").into())
}

fn altered(frame: SnapshotFrame) -> SnapshotFrame {
    match frame {
        SnapshotFrame::Part { identity, index, bytes } => {
            let mut changed = bytes.to_vec();
            if let Some(first) = changed.first_mut() {
                *first = first.wrapping_add(1);
            }
            SnapshotFrame::Part {
                identity,
                index,
                bytes: Bytes::from(changed),
            }
        }
        end => end,
    }
}

fn interleave(left: Vec<SnapshotFrame>, right: Vec<SnapshotFrame>) -> Vec<SnapshotFrame> {
    let mut left = left.into_iter();
    let mut right = right.into_iter();
    let mut mixed = Vec::new();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return mixed,
            (first, second) => mixed.extend(first.into_iter().chain(second)),
        }
    }
}

fn retotaled(manifest: &SnapshotManifest, delta: i64) -> Result<SnapshotManifest, BoxError> {
    let mut raw = serde_json::to_value(manifest)?;
    let total = raw["total_bytes"].as_u64().ok_or("manifest without a byte total")?;
    raw["total_bytes"] = json!(total.checked_add_signed(delta).ok_or("byte total out of range")?);
    Ok(serde_json::from_value(raw)?)
}

fn assert_no_snapshot(events: &mut broadcast::Receiver<ReaderEvent>) -> TestResult {
    loop {
        match events.try_recv() {
            Ok(ReaderEvent::Snapshot { state, .. }) => {
                return Err(format!("an invalid assembly installed {} members", state.iter().count()).into())
            }
            Ok(ReaderEvent::Diff(_)) => {}
            Err(broadcast::error::TryRecvError::Empty) => return Ok(()),
            Err(err) => return Err(err.into()),
        }
    }
}

struct ChunkedReader {
    runtime: FakeRuntime,
    reader: PresenceReader,
    events: broadcast::Receiver<ReaderEvent>,
    limits: SnapshotLimits,
    at: GenerationEpoch,
    baseline: Presences,
}

impl ChunkedReader {
    async fn start(server: &NatsServer) -> Result<Self, BoxError> {
        let mut runtime = FakeRuntime::start(server).await?;
        let limits = chunked_limits()?;
        let gate = AssemblyGate::new(limits.with_per_connection(AssembliesPerConnection::try_from(1)?));
        let options = ReaderOptions::default()
            .with_limits(limits)
            .with_resnapshot(ResnapshotInterval::try_from(BASELINE_RESNAPSHOT)?)
            .with_gate(gate);
        let reader = runtime.reader(options).await?;
        let mut events = reader.events();
        let at = epoch(1, 10);
        let baseline = state(&[("ana", ANA_REF)])?;
        runtime.serve(at, 2, &baseline).await?;
        let (_, installed) = snapshot_event(&mut events).await?;
        assert_eq!(installed, baseline);
        Ok(Self {
            runtime,
            reader,
            events,
            limits,
            at,
            baseline,
        })
    }

    async fn next_capture(&mut self, label: CrowdLabel) -> Result<(Request, CapturedSnapshot), BoxError> {
        let request = self.runtime.request().await?;
        let captured = self.capture_for(&request, label)?;
        Ok((request, captured))
    }

    fn capture_for(&self, request: &Request, label: CrowdLabel) -> Result<CapturedSnapshot, BoxError> {
        let captured = self
            .runtime
            .capture_in(request, self.at, 3, &crowd(label)?, self.limits)?;
        let parts = parts_of(&captured.frames());
        assert!(parts >= 3, "the crowd must span several parts, got {parts}");
        Ok(captured)
    }

    async fn expect_abandoned(&mut self, failed: &Request) -> Result<Request, BoxError> {
        let retry = self.runtime.request().await?;
        assert_ne!(retry.id, failed.id, "a retry must carry a fresh request id");
        assert_no_snapshot(&mut self.events)?;
        assert_eq!(
            self.reader.presences(),
            self.baseline,
            "an abandoned assembly must leave the installed view untouched"
        );
        Ok(retry)
    }

    async fn expect_recovery(&mut self, retry: &Request) -> TestResult {
        let label = CrowdLabel("recovered");
        let captured = self.capture_for(retry, label)?;
        self.runtime
            .respond(retry, captured.manifest(), captured.frames())
            .await?;
        let (_, installed) = snapshot_event(&mut self.events).await?;
        assert_eq!(installed, crowd(label)?);
        assert_eq!(self.reader.presences(), installed);
        Ok(())
    }

    async fn close(self) {
        self.reader.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_part_abandons_at_the_deadline_and_the_retry_installs() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, captured) = chunked.next_capture(CrowdLabel("partial")).await?;
    let started = tokio::time::Instant::now();
    chunked
        .runtime
        .respond(
            &request,
            captured.manifest(),
            without_part(captured.frames(), PartIndex::from(2)),
        )
        .await?;

    let retry = chunked.expect_abandoned(&request).await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= DEADLINE_FLOOR,
        "a missing part must wait for the assembly deadline, retried after {elapsed:?}"
    );
    chunked.expect_recovery(&retry).await?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflicting_duplicate_part_abandons_and_the_retry_installs() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, captured) = chunked.next_capture(CrowdLabel("conflict")).await?;
    let mut frames = captured.frames();
    let duplicate = altered(only_part(&frames, PartIndex::from(2))?);
    frames.insert(2, duplicate);
    let started = tokio::time::Instant::now();
    chunked.runtime.respond(&request, captured.manifest(), frames).await?;

    let retry = chunked.expect_abandoned(&request).await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed < CHUNK_DEADLINE,
        "a conflicting duplicate must abandon at once, retried after {elapsed:?}"
    );
    chunked.expect_recovery(&retry).await?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identical_duplicate_part_is_idempotent() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let label = CrowdLabel("echo");
    let (request, captured) = chunked.next_capture(label).await?;
    let mut frames = captured.frames();
    let duplicate = only_part(&frames, PartIndex::from(2))?;
    frames.insert(2, duplicate);
    chunked.runtime.respond(&request, captured.manifest(), frames).await?;

    let (_, installed) = snapshot_event(&mut chunked.events).await?;
    assert_eq!(installed, crowd(label)?);
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_part_after_install_changes_nothing() -> TestResult {
    let server = server_or_skip!();
    let mut runtime = FakeRuntime::start(&server).await?;
    let limits = chunked_limits()?;
    let reader = runtime.reader(ReaderOptions::default().with_limits(limits)).await?;
    let mut events = reader.events();
    let request = runtime.request().await?;
    let presences = crowd(CrowdLabel("settled"))?;
    let captured = runtime.capture_in(&request, epoch(1, 10), 3, &presences, limits)?;
    runtime
        .respond(&request, captured.manifest(), captured.frames())
        .await?;
    let (_, installed) = snapshot_event(&mut events).await?;
    assert_eq!(installed, presences);

    let frames = captured.frames();
    let late = vec![
        altered(only_part(&frames, PartIndex::from(1))?),
        only_part(&frames, PartIndex::from(2))?,
        frames.last().cloned().ok_or("no end frame")?,
    ];
    runtime.publish(late).await?;

    assert!(
        runtime.next_request(QUIET).await?.is_none(),
        "a late part must not restart an installed assembly"
    );
    assert_no_snapshot(&mut events)?;
    assert_eq!(reader.presences(), presences);
    reader.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_part_after_the_deadline_cannot_complete_the_abandoned_assembly() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, captured) = chunked.next_capture(CrowdLabel("stale")).await?;
    let missing = PartIndex::from(2);
    let frames = captured.frames();
    chunked
        .runtime
        .respond(&request, captured.manifest(), without_part(frames.clone(), missing))
        .await?;

    let retry = chunked.expect_abandoned(&request).await?;
    chunked.runtime.publish(vec![only_part(&frames, missing)?]).await?;
    assert_no_snapshot(&mut chunked.events)?;
    chunked.expect_recovery(&retry).await?;
    assert_no_snapshot(&mut chunked.events)?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn parts_of_an_abandoned_request_interleaved_with_the_retry_are_ignored() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, stale) = chunked.next_capture(CrowdLabel("stale")).await?;
    let retry = chunked.expect_abandoned(&request).await?;

    let label = CrowdLabel("fresh");
    let fresh = chunked.capture_for(&retry, label)?;
    chunked
        .runtime
        .respond(&retry, fresh.manifest(), interleave(stale.frames(), fresh.frames()))
        .await?;
    let (_, installed) = snapshot_event(&mut chunked.events).await?;
    assert_eq!(installed, crowd(label)?);
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_snapshot_id_for_the_same_request_abandons_the_assembly() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, announced) = chunked.next_capture(CrowdLabel("announced")).await?;
    let impostor = chunked.capture_for(&request, CrowdLabel("impostor"))?;
    assert_ne!(
        announced.manifest().identity().snapshot(),
        impostor.manifest().identity().snapshot()
    );
    chunked
        .runtime
        .respond(
            &request,
            announced.manifest(),
            interleave(impostor.frames(), announced.frames()),
        )
        .await?;

    let retry = chunked.expect_abandoned(&request).await?;
    chunked.expect_recovery(&retry).await?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_end_abandons_at_the_deadline() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, captured) = chunked.next_capture(CrowdLabel("endless")).await?;
    let started = tokio::time::Instant::now();
    chunked
        .runtime
        .respond(&request, captured.manifest(), without_end(captured.frames()))
        .await?;

    let retry = chunked.expect_abandoned(&request).await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= DEADLINE_FLOOR,
        "every part without an end must wait for the deadline, retried after {elapsed:?}"
    );
    chunked.expect_recovery(&retry).await?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_request_retries_after_the_deadline() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let request = chunked.runtime.request().await?;
    let started = tokio::time::Instant::now();

    let retry = chunked.expect_abandoned(&request).await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed >= DEADLINE_FLOOR,
        "an unanswered request must hold until the deadline, retried after {elapsed:?}"
    );
    chunked.expect_recovery(&retry).await?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_end_with_another_part_count_abandons() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    let (request, captured) = chunked.next_capture(CrowdLabel("miscounted")).await?;
    let frames = captured
        .frames()
        .into_iter()
        .map(|frame| match frame {
            SnapshotFrame::End { identity, parts } => SnapshotFrame::End {
                identity,
                parts: (parts.get() + 1).into(),
            },
            part => part,
        })
        .collect();
    chunked.runtime.respond(&request, captured.manifest(), frames).await?;

    let retry = chunked.expect_abandoned(&request).await?;
    chunked.expect_recovery(&retry).await?;
    chunked.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_byte_total_unlike_the_parts_abandons() -> TestResult {
    let server = server_or_skip!();
    let mut chunked = ChunkedReader::start(&server).await?;
    for delta in [1, -1] {
        let (request, captured) = chunked.next_capture(CrowdLabel("resized")).await?;
        chunked
            .runtime
            .respond(&request, &retotaled(captured.manifest(), delta)?, captured.frames())
            .await?;
        let retry = chunked.expect_abandoned(&request).await?;
        chunked.expect_recovery(&retry).await?;
        chunked.baseline = chunked.reader.presences();
    }
    chunked.close().await;
    Ok(())
}
