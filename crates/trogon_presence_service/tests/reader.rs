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
use trogon_presence_service::snapshot::{CapturedSnapshot, SnapshotFrame, SnapshotIdentity};
use trogon_presence_service::subjects::{diff_subject, SnapshotReplySubject};
use trogon_presence_service::{
    PresenceReader, ReaderEvent, ReaderIdentity, ReaderOptions, ResnapshotInterval, SnapshotLimits,
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
        let identity = SnapshotIdentity::new(request.id, SnapshotId::generate()?, at, DiffSequence::from(seq));
        Ok(CapturedSnapshot::capture(
            identity,
            presences,
            SnapshotLimits::default(),
        )?)
    }

    async fn respond(&self, request: &Request, captured: &CapturedSnapshot, frames: Vec<SnapshotFrame>) -> TestResult {
        let manifest = captured.manifest();
        let identity = manifest.identity();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_CODE, "ok");
        headers.insert(HEADER_KIND, "manifest");
        headers.insert(HEADER_SEQ, identity.seq().get().to_string());
        epoch_headers(&mut headers, identity.epoch());
        self.client
            .publish_with_headers(request.inbox.clone(), headers, serde_json::to_vec(manifest)?.into())
            .await?;
        let reply = SnapshotReplySubject::new(&self.caller, &self.connection, &identity.snapshot());
        for frame in frames {
            frame.publish(&self.client, &reply).await?;
        }
        self.client.flush().await?;
        Ok(())
    }

    async fn serve(&mut self, at: GenerationEpoch, seq: u64, presences: &Presences) -> TestResult {
        let request = self.request().await?;
        let captured = self.capture(&request, at, seq, presences)?;
        self.respond(&request, &captured, captured.frames()).await
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
    runtime.respond(&request, &captured, captured.frames()).await?;

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
    runtime.respond(&request, &captured, tampered).await?;

    let retry = runtime.request().await?;
    assert_ne!(retry.id, request.id, "a retry must carry a fresh request id");
    let captured = runtime.capture(&retry, at, 3, &presences)?;
    runtime.respond(&retry, &captured, captured.frames()).await?;
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
