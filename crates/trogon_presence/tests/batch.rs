mod common;

use std::time::Duration;

use async_nats::jetstream::message::StreamMessage;
use async_nats::jetstream::stream::{LastRawMessageErrorKind, Stream};
use async_nats::{jetstream, Subject};
use bytes::Bytes;
use common::NatsServer;
use trogon_presence::{
    AtomicBatch, BatchBudget, BatchOutcome, BatchPacer, BatchPosition, BatchRecord, EntryRevision, Expected, GuardTtl,
    Presence, PresenceConfig, ProvisionOptions,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const STALL: Duration = Duration::from_millis(1200);

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

struct Bucket {
    client: async_nats::Client,
    config: PresenceConfig,
    stream: Stream,
}

impl Bucket {
    async fn provision(server: &NatsServer) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = server.client().await;
        let config = PresenceConfig::default();
        Presence::provision(client.clone(), config.clone(), ProvisionOptions::default())
            .await?
            .close();
        let stream = jetstream::new(client.clone())
            .get_stream(config.bucket().stream_name())
            .await?;
        Ok(Self { client, config, stream })
    }

    fn subject(&self, key: &str) -> Subject {
        Subject::from(format!("$KV.{}.{key}", self.config.bucket()))
    }

    async fn read(&self, subject: &Subject) -> Result<Option<StreamMessage>, Box<dyn std::error::Error + Send + Sync>> {
        match self.stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => Ok(Some(message)),
            Err(err) if err.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn guard(&self, key: &str, expected: Expected) -> BatchRecord {
        BatchRecord::put(self.subject(key), Bytes::from_static(b"guard"), expected).with_ttl(GuardTtl::default().into())
    }

    fn put(&self, key: &str, payload: &'static [u8]) -> BatchRecord {
        BatchRecord::put(self.subject(key), Bytes::from_static(payload), Expected::Empty)
    }
}

fn committed(outcome: BatchOutcome) -> Result<trogon_presence::BatchAck, String> {
    match outcome {
        BatchOutcome::Committed(ack) => Ok(ack),
        other => Err(format!("expected a commit, got {other:?}")),
    }
}

fn position(index: u16) -> Result<BatchPosition, String> {
    BatchPosition::new(index).ok_or_else(|| format!("position {index} is not valid"))
}

#[tokio::test]
async fn guard_receipt_and_entry_commit_at_the_final_sequence() -> TestResult {
    let server = server_or_skip!();
    let bucket = Bucket::provision(&server).await?;
    let mut batch = AtomicBatch::new()?;
    batch.push(bucket.guard("g.room", Expected::Empty))?;
    batch.push(bucket.put("r.op1", b"receipt"))?;
    let entry = batch.push(bucket.put("e.room.ana", b"entry"))?;
    let ack = committed(batch.publish(&bucket.client).await?)?;
    assert_eq!(ack.last(), entry);
    let stored = bucket
        .read(&bucket.subject("e.room.ana"))
        .await?
        .ok_or("entry missing")?;
    assert_eq!(EntryRevision::from(stored.sequence), ack.final_sequence());
    assert_eq!(ack.revision_of(entry)?, ack.final_sequence());
    let guard = bucket.read(&bucket.subject("g.room")).await?.ok_or("guard missing")?;
    assert_eq!(EntryRevision::from(guard.sequence), ack.revision_of(position(1)?)?);
    Ok(())
}

#[tokio::test]
async fn stale_guard_rejects_the_whole_batch_with_wrong_last_sequence() -> TestResult {
    let server = server_or_skip!();
    let bucket = Bucket::provision(&server).await?;
    let mut seed = AtomicBatch::new()?;
    seed.push(bucket.guard("g.room", Expected::Empty))?;
    let seeded = committed(seed.publish(&bucket.client).await?)?.final_sequence();

    let mut batch = AtomicBatch::new()?;
    batch.push(bucket.guard("g.room", Expected::Empty))?;
    batch.push(bucket.put("r.op2", b"receipt"))?;
    batch.push(bucket.put("e.room.bea", b"entry"))?;
    match batch.publish(&bucket.client).await? {
        BatchOutcome::Rejected(rejection) => {
            assert!(rejection.is_wrong_last_sequence(), "unexpected rejection {rejection:?}")
        }
        other => return Err(format!("expected a rejection, got {other:?}").into()),
    }
    assert!(bucket.read(&bucket.subject("r.op2")).await?.is_none());
    assert!(bucket.read(&bucket.subject("e.room.bea")).await?.is_none());
    let guard = bucket.read(&bucket.subject("g.room")).await?.ok_or("guard missing")?;
    assert_eq!(EntryRevision::from(guard.sequence), seeded);
    Ok(())
}

#[tokio::test]
async fn purge_with_a_body_commits_atomically() -> TestResult {
    let server = server_or_skip!();
    let bucket = Bucket::provision(&server).await?;
    let mut seed = AtomicBatch::new()?;
    seed.push(bucket.put("e.room.cy", b"entry"))?;
    let seeded = committed(seed.publish(&bucket.client).await?)?.final_sequence();

    let mut batch = AtomicBatch::new()?;
    batch.push(bucket.guard("g.room", Expected::Empty))?;
    let purge = batch.push(BatchRecord::purge(
        bucket.subject("e.room.cy"),
        Bytes::from_static(b"tombstone"),
        Expected::At(seeded),
    ))?;
    let ack = committed(batch.publish(&bucket.client).await?)?;
    let stored = bucket
        .read(&bucket.subject("e.room.cy"))
        .await?
        .ok_or("tombstone missing")?;
    assert_eq!(EntryRevision::from(stored.sequence), ack.revision_of(purge)?);
    assert_eq!(stored.payload.as_ref(), b"tombstone");
    assert_eq!(
        stored.headers.get("KV-Operation").map(|value| value.as_str()),
        Some("PURGE")
    );
    Ok(())
}

#[tokio::test]
async fn a_sixty_six_record_batch_commits() -> TestResult {
    let server = server_or_skip!();
    let bucket = Bucket::provision(&server).await?;
    let mut batch = AtomicBatch::new()?;
    for index in 0..66 {
        batch.push(bucket.put(&format!("e.room.k{index}"), b"entry"))?;
    }
    let ack = committed(batch.publish(&bucket.client).await?)?;
    assert_eq!(ack.last(), position(66)?);
    for index in [0u16, 65] {
        let stored = bucket
            .read(&bucket.subject(&format!("e.room.k{index}")))
            .await?
            .ok_or("record missing")?;
        assert_eq!(
            EntryRevision::from(stored.sequence),
            ack.revision_of(position(index + 1)?)?
        );
    }
    Ok(())
}

struct StallBefore(BatchPosition);

impl BatchPacer for StallBefore {
    async fn before_send(&self, position: BatchPosition) {
        if position == self.0 {
            tokio::time::sleep(STALL).await;
        }
    }
}

#[tokio::test]
async fn a_sender_stalling_past_the_send_budget_is_unknown_without_partial_writes() -> TestResult {
    let server = server_or_skip!();
    let bucket = Bucket::provision(&server).await?;
    let mut batch = AtomicBatch::new()?;
    batch.push(bucket.guard("g.room", Expected::Empty))?;
    batch.push(bucket.put("r.op3", b"receipt"))?;
    batch.push(bucket.put("e.room.dee", b"entry"))?;
    let outcome = batch
        .publish_with(&bucket.client, BatchBudget::default(), &StallBefore(position(2)?))
        .await?;
    assert!(matches!(outcome, BatchOutcome::Unknown), "unexpected {outcome:?}");
    for key in ["g.room", "r.op3", "e.room.dee"] {
        assert!(bucket.read(&bucket.subject(key)).await?.is_none(), "{key} was written");
    }
    Ok(())
}
