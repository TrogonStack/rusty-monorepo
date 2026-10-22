use std::collections::BTreeSet;
use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_nats::header::{
    NATS_BATCH_COMMIT, NATS_BATCH_COMMIT_FINAL, NATS_BATCH_ID, NATS_BATCH_SEQUENCE,
    NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, NATS_MESSAGE_TTL, NATS_ROLLUP,
};
use async_nats::jetstream::ErrorCode;
use async_nats::{HeaderMap, Request, RequestErrorKind, Subject};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::config::MessageTtl;
use crate::constants::{
    BATCH_MAX_BYTES, BATCH_MAX_MESSAGES, BATCH_SEND_BUDGET, BATCH_TOTAL_BUDGET, KV_OPERATION_HEADER,
    KV_OPERATION_PURGE, NATS_HEADER_LINE_OVERHEAD, NATS_HEADER_PREAMBLE, NATS_HEADER_TERMINATOR, ROLLUP_SUBJECT,
};
use crate::domain::JetStreamRoute;
use crate::entropy::EntropyError;
use crate::position::{BatchId, PositionOverflow};
use crate::revision::EntryRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expected {
    Empty,
    At(EntryRevision),
}

impl Expected {
    fn header_value(self) -> String {
        match self {
            Self::Empty => "0".to_owned(),
            Self::At(revision) => revision.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOp {
    Put,
    Purge,
}

pub(crate) fn record_headers(op: RecordOp, ttl: Option<MessageTtl>, expected: Expected) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if op == RecordOp::Purge {
        headers.insert(KV_OPERATION_HEADER, KV_OPERATION_PURGE);
        headers.insert(NATS_ROLLUP, ROLLUP_SUBJECT);
    }
    if let Some(ttl) = ttl {
        headers.insert(NATS_MESSAGE_TTL, ttl.header_value());
    }
    headers.insert(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE, expected.header_value());
    headers
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRecord {
    subject: Subject,
    payload: Bytes,
    ttl: Option<MessageTtl>,
    expected: Expected,
    op: RecordOp,
}

impl BatchRecord {
    pub fn put(subject: Subject, payload: Bytes, expected: Expected) -> Self {
        Self {
            subject,
            payload,
            ttl: None,
            expected,
            op: RecordOp::Put,
        }
    }

    pub fn purge(subject: Subject, payload: Bytes, expected: Expected) -> Self {
        Self {
            subject,
            payload,
            ttl: None,
            expected,
            op: RecordOp::Purge,
        }
    }

    pub fn with_ttl(self, ttl: MessageTtl) -> Self {
        Self { ttl: Some(ttl), ..self }
    }

    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    pub fn op(&self) -> RecordOp {
        self.op
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct BatchPosition(u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("batch positions start at 1, got {0}")]
pub struct BatchPositionError(u16);

impl TryFrom<u16> for BatchPosition {
    type Error = BatchPositionError;

    fn try_from(position: u16) -> Result<Self, Self::Error> {
        Self::new(position).ok_or(BatchPositionError(position))
    }
}

impl From<BatchPosition> for u16 {
    fn from(position: BatchPosition) -> Self {
        position.0
    }
}

impl BatchPosition {
    pub const FIRST: Self = Self(1);

    pub fn new(position: u16) -> Option<Self> {
        (position >= 1).then_some(Self(position))
    }

    pub fn get(self) -> u16 {
        self.0
    }

    pub fn following(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchAck {
    final_sequence: EntryRevision,
    last: BatchPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BatchRevisionError {
    #[error("batch position {position} is past the committed count {last}")]
    OutOfRange { position: u16, last: u16 },
    #[error(transparent)]
    Overflow(#[from] PositionOverflow),
}

impl BatchAck {
    pub fn final_sequence(self) -> EntryRevision {
        self.final_sequence
    }

    pub fn last(self) -> BatchPosition {
        self.last
    }

    pub fn revision_of(self, position: BatchPosition) -> Result<EntryRevision, BatchRevisionError> {
        let distance = self
            .last
            .0
            .checked_sub(position.0)
            .ok_or(BatchRevisionError::OutOfRange {
                position: position.0,
                last: self.last.0,
            })?;
        Ok(self.final_sequence.checked_sub(u64::from(distance))?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRejection {
    code: ErrorCode,
    description: String,
}

impl BatchRejection {
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn is_wrong_last_sequence(&self) -> bool {
        self.code == ErrorCode::STREAM_WRONG_LAST_SEQUENCE
    }

    /// True only for the server's inflight-pressure rejections, which commit nothing and assign no sequence.
    pub(crate) fn is_inflight_pressure(&self) -> bool {
        INFLIGHT_PRESSURE_DESCRIPTIONS.contains(&self.description.as_str())
    }
}

const INFLIGHT_PRESSURE_DESCRIPTIONS: [&str; 2] =
    ["atomic publish too many inflight", "atomic publish batch is incomplete"];
const DEFAULT_INFLIGHT_BATCH_LIMIT: usize = 16;
const MAX_INFLIGHT_BATCH_LIMIT: usize = 1024;
const PRESSURE_MAX_ATTEMPTS: u32 = 5;
const PRESSURE_BACKOFF_BASE: Duration = Duration::from_millis(20);
const PRESSURE_BACKOFF_CAP: Duration = Duration::from_millis(250);

/// How many atomic batches one process keeps open against JetStream at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InflightBatchLimit(usize);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InflightBatchLimitError {
    #[error("inflight batch limit must be between 1 and {MAX_INFLIGHT_BATCH_LIMIT}, got {0}")]
    OutOfRange(usize),
    #[error("inflight batch limit must be a whole number, got {0:?}")]
    NotANumber(String),
}

impl InflightBatchLimit {
    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for InflightBatchLimit {
    fn default() -> Self {
        Self(DEFAULT_INFLIGHT_BATCH_LIMIT)
    }
}

impl TryFrom<usize> for InflightBatchLimit {
    type Error = InflightBatchLimitError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if (1..=MAX_INFLIGHT_BATCH_LIMIT).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InflightBatchLimitError::OutOfRange(value))
        }
    }
}

impl FromStr for InflightBatchLimit {
    type Err = InflightBatchLimitError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        raw.trim()
            .parse::<usize>()
            .map_err(|_| InflightBatchLimitError::NotANumber(raw.to_owned()))
            .and_then(Self::try_from)
    }
}

impl std::fmt::Display for InflightBatchLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A shared gate that caps the atomic batches a process has open; clones share the same permits.
#[derive(Debug, Clone)]
pub struct InflightBatches {
    limit: InflightBatchLimit,
    permits: Arc<Semaphore>,
}

impl InflightBatches {
    pub fn new(limit: InflightBatchLimit) -> Self {
        Self {
            limit,
            permits: Arc::new(Semaphore::new(limit.get())),
        }
    }

    /// The gate every sink built without an explicit gate shares, sized by the default limit.
    pub fn process_default() -> Self {
        static PROCESS: OnceLock<InflightBatches> = OnceLock::new();
        PROCESS.get_or_init(|| Self::new(InflightBatchLimit::default())).clone()
    }

    pub fn limit(&self) -> InflightBatchLimit {
        self.limit
    }

    async fn admit(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.permits).acquire_owned().await.ok()
    }
}

fn pressure_backoff(retry: u32) -> Duration {
    let ceiling = PRESSURE_BACKOFF_BASE
        .saturating_mul(1u32.checked_shl(retry).unwrap_or(u32::MAX))
        .min(PRESSURE_BACKOFF_CAP);
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return ceiling;
    }
    let ceiling_micros = u64::try_from(ceiling.as_micros()).unwrap_or(u64::MAX);
    Duration::from_micros(u64::from_le_bytes(bytes) % ceiling_micros.saturating_add(1))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    Committed(BatchAck),
    Rejected(BatchRejection),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    #[error("subject {0} appears twice in one atomic batch")]
    DuplicateSubject(Subject),
    #[error("an atomic batch holds at most {BATCH_MAX_MESSAGES} records")]
    TooManyRecords,
    #[error("atomic batch would encode to {encoded} bytes, the limit is {BATCH_MAX_BYTES}")]
    TooLarge { encoded: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum BatchPublishError {
    #[error("an empty atomic batch cannot be published")]
    Empty,
    #[error("no stream listens on {0}")]
    NoStream(Subject),
    #[error("the batch could not be admitted: {0}")]
    Admission(async_nats::RequestError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBudget {
    send: Duration,
    total: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("batch send budget {send:?} must be positive and within the total budget {total:?}")]
pub struct BatchBudgetError {
    send: Duration,
    total: Duration,
}

impl BatchBudget {
    pub fn new(send: Duration, total: Duration) -> Result<Self, BatchBudgetError> {
        if send.is_zero() || send > total {
            Err(BatchBudgetError { send, total })
        } else {
            Ok(Self { send, total })
        }
    }

    pub fn send(self) -> Duration {
        self.send
    }

    pub fn total(self) -> Duration {
        self.total
    }
}

impl Default for BatchBudget {
    fn default() -> Self {
        Self {
            send: BATCH_SEND_BUDGET,
            total: BATCH_TOTAL_BUDGET,
        }
    }
}

pub trait BatchPacer: Sync {
    fn before_send(&self, position: BatchPosition) -> impl Future<Output = ()> + Send;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Unpaced;

impl BatchPacer for Unpaced {
    async fn before_send(&self, _position: BatchPosition) {}
}

#[derive(Debug, Clone)]
pub struct AtomicBatch {
    id: BatchId,
    records: Vec<BatchRecord>,
    subjects: BTreeSet<Subject>,
    encoded: usize,
}

struct Outbound {
    subject: Subject,
    headers: HeaderMap,
    payload: Bytes,
}

fn encoded_headers_len(headers: &HeaderMap) -> usize {
    let lines: usize = headers
        .iter()
        .map(|(name, values)| {
            let name: &str = name.as_ref();
            values
                .iter()
                .map(|value| name.len() + value.as_str().len() + NATS_HEADER_LINE_OVERHEAD)
                .sum::<usize>()
        })
        .sum();
    NATS_HEADER_PREAMBLE.len() + lines + NATS_HEADER_TERMINATOR.len()
}

fn commit_line_len() -> usize {
    let name: &str = NATS_BATCH_COMMIT.as_ref();
    name.len() + NATS_BATCH_COMMIT_FINAL.len() + NATS_HEADER_LINE_OVERHEAD
}

impl AtomicBatch {
    pub fn new() -> Result<Self, EntropyError> {
        BatchId::generate().map(Self::with_id)
    }

    pub fn with_id(id: BatchId) -> Self {
        Self {
            id,
            records: Vec::new(),
            subjects: BTreeSet::new(),
            encoded: commit_line_len(),
        }
    }

    pub fn id(&self) -> BatchId {
        self.id
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn encoded_len(&self) -> usize {
        self.encoded
    }

    pub fn next_position(&self) -> BatchPosition {
        BatchPosition(self.records.len() as u16 + 1)
    }

    pub fn push(&mut self, record: BatchRecord) -> Result<BatchPosition, BatchError> {
        if self.records.len() >= BATCH_MAX_MESSAGES {
            return Err(BatchError::TooManyRecords);
        }
        if self.subjects.contains(&record.subject) {
            return Err(BatchError::DuplicateSubject(record.subject));
        }
        let position = BatchPosition(self.records.len() as u16 + 1);
        let encoded =
            self.encoded + encoded_headers_len(&self.headers_for(&record, position, false)) + record.payload.len();
        if encoded > BATCH_MAX_BYTES {
            return Err(BatchError::TooLarge { encoded });
        }
        self.encoded = encoded;
        self.subjects.insert(record.subject.clone());
        self.records.push(record);
        Ok(position)
    }

    fn headers_for(&self, record: &BatchRecord, position: BatchPosition, commit: bool) -> HeaderMap {
        let mut headers = record_headers(record.op, record.ttl, record.expected);
        headers.insert(NATS_BATCH_ID, self.id.to_string());
        headers.insert(NATS_BATCH_SEQUENCE, position.0.to_string());
        if commit {
            headers.insert(NATS_BATCH_COMMIT, NATS_BATCH_COMMIT_FINAL);
        }
        headers
    }

    fn outbound(&self, route: &JetStreamRoute) -> Vec<Outbound> {
        let last = self.records.len();
        self.records
            .iter()
            .enumerate()
            .map(|(index, record)| Outbound {
                subject: route.publish_subject(&record.subject),
                headers: self.headers_for(record, BatchPosition(index as u16 + 1), index + 1 == last),
                payload: record.payload.clone(),
            })
            .collect()
    }

    pub async fn publish(self, client: &async_nats::Client) -> Result<BatchOutcome, BatchPublishError> {
        self.publish_with(client, BatchBudget::default(), &Unpaced).await
    }

    pub async fn publish_with<P: BatchPacer>(
        self,
        client: &async_nats::Client,
        budget: BatchBudget,
        pacer: &P,
    ) -> Result<BatchOutcome, BatchPublishError> {
        self.publish_routed(client, &JetStreamRoute::local(), budget, pacer)
            .await
    }

    /// Publishes every record on the subject `route` maps its stored subject to.
    ///
    /// A batch the server turns away for inflight pressure committed nothing, so it is resent under a fresh
    /// batch id with jittered backoff, a bounded number of times and only within the total budget.
    pub async fn publish_routed<P: BatchPacer>(
        self,
        client: &async_nats::Client,
        route: &JetStreamRoute,
        budget: BatchBudget,
        pacer: &P,
    ) -> Result<BatchOutcome, BatchPublishError> {
        self.publish_paced(client, route, budget, pacer, None).await
    }

    /// Like [`AtomicBatch::publish_routed`], holding one permit of `gate` while each attempt is open.
    pub async fn publish_gated<P: BatchPacer>(
        self,
        client: &async_nats::Client,
        route: &JetStreamRoute,
        budget: BatchBudget,
        pacer: &P,
        gate: &InflightBatches,
    ) -> Result<BatchOutcome, BatchPublishError> {
        self.publish_paced(client, route, budget, pacer, Some(gate)).await
    }

    async fn publish_paced<P: BatchPacer>(
        mut self,
        client: &async_nats::Client,
        route: &JetStreamRoute,
        budget: BatchBudget,
        pacer: &P,
        gate: Option<&InflightBatches>,
    ) -> Result<BatchOutcome, BatchPublishError> {
        let mut retries = 0u32;
        let mut deadline = None;
        loop {
            let permit = match gate {
                Some(gate) => gate.admit().await,
                None => None,
            };
            let finish_by = *deadline.get_or_insert_with(|| Instant::now() + budget.total);
            let outcome = self.attempt(client, route, budget, finish_by, pacer).await;
            drop(permit);
            let rejection = match outcome {
                Ok(BatchOutcome::Rejected(rejection)) if rejection.is_inflight_pressure() => rejection,
                other => return other,
            };
            retries += 1;
            let delay = pressure_backoff(retries - 1);
            if retries >= PRESSURE_MAX_ATTEMPTS || Instant::now() + delay >= finish_by {
                return Ok(BatchOutcome::Rejected(rejection));
            }
            let Ok(fresh) = BatchId::generate() else {
                return Ok(BatchOutcome::Rejected(rejection));
            };
            tracing::debug!(batch = %self.id, retry = retries, ?delay, reason = rejection.description(), "atomic batch turned away for inflight pressure, resending");
            tokio::time::sleep(delay).await;
            self.id = fresh;
        }
    }

    async fn attempt<P: BatchPacer>(
        &self,
        client: &async_nats::Client,
        route: &JetStreamRoute,
        budget: BatchBudget,
        finish_by: Instant,
        pacer: &P,
    ) -> Result<BatchOutcome, BatchPublishError> {
        let remaining = finish_by.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(BatchOutcome::Unknown);
        }
        let last = BatchPosition(self.records.len() as u16);
        let mut messages = self.outbound(route).into_iter();
        let Some(first) = messages.next() else {
            return Err(BatchPublishError::Empty);
        };
        if last == BatchPosition::FIRST {
            return self.commit(client, first, finish_by, BatchPosition::FIRST).await;
        }
        let admitted = match client
            .send_request(
                first.subject.clone(),
                Request::new()
                    .headers(first.headers)
                    .payload(first.payload)
                    .timeout(Some(remaining)),
            )
            .await
        {
            Ok(reply) if reply.payload.is_empty() => Instant::now(),
            Ok(reply) => return Ok(rejection_or_unknown(&reply.payload)),
            Err(err) => {
                return match err.kind() {
                    RequestErrorKind::NoResponders => Err(BatchPublishError::NoStream(first.subject)),
                    RequestErrorKind::InvalidSubject | RequestErrorKind::MaxPayloadExceeded => {
                        Err(BatchPublishError::Admission(err))
                    }
                    RequestErrorKind::TimedOut | RequestErrorKind::Other => Ok(BatchOutcome::Unknown),
                }
            }
        };
        let send_by = (admitted + budget.send).min(finish_by);
        for (index, message) in messages.enumerate() {
            let position = BatchPosition(index as u16 + 2);
            pacer.before_send(position).await;
            if Instant::now() > send_by {
                tracing::warn!(batch = %self.id, position = position.0, "atomic batch exceeded its send budget, abandoning");
                return Ok(BatchOutcome::Unknown);
            }
            if position == last {
                return self.commit(client, message, finish_by, position).await;
            }
            if client
                .publish_with_headers(message.subject, message.headers, message.payload)
                .await
                .is_err()
            {
                return Ok(BatchOutcome::Unknown);
            }
        }
        Ok(BatchOutcome::Unknown)
    }

    /// A `NoResponders` on the first position proves nothing reached a stream; later positions stay ambiguous.
    async fn commit(
        &self,
        client: &async_nats::Client,
        message: Outbound,
        finish_by: Instant,
        position: BatchPosition,
    ) -> Result<BatchOutcome, BatchPublishError> {
        let remaining = finish_by.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(BatchOutcome::Unknown);
        }
        let subject = message.subject.clone();
        let reply = client
            .send_request(
                message.subject,
                Request::new()
                    .headers(message.headers)
                    .payload(message.payload)
                    .timeout(Some(remaining)),
            )
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(err) if position == BatchPosition::FIRST && err.kind() == RequestErrorKind::NoResponders => {
                return Err(BatchPublishError::NoStream(subject));
            }
            Err(_) => return Ok(BatchOutcome::Unknown),
        };
        let Ok(body) = serde_json::from_slice::<AckBody>(&reply.payload) else {
            return Ok(BatchOutcome::Unknown);
        };
        if let Some(error) = body.error {
            return Ok(BatchOutcome::Rejected(error.into()));
        }
        let expected_batch = self.id.to_string();
        let count_matches = body.count == Some(self.records.len() as u64);
        match (body.seq, body.batch) {
            (Some(seq), Some(batch)) if batch == expected_batch && count_matches => {
                Ok(BatchOutcome::Committed(BatchAck {
                    final_sequence: EntryRevision::from(seq),
                    last: BatchPosition(self.records.len() as u16),
                }))
            }
            _ => Ok(BatchOutcome::Unknown),
        }
    }
}

fn rejection_or_unknown(payload: &[u8]) -> BatchOutcome {
    match serde_json::from_slice::<AckBody>(payload) {
        Ok(AckBody { error: Some(error), .. }) => BatchOutcome::Rejected(error.into()),
        _ => BatchOutcome::Unknown,
    }
}

#[derive(Deserialize)]
struct AckBody {
    #[serde(default)]
    error: Option<ApiErrorBody>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    batch: Option<String>,
    #[serde(default)]
    count: Option<u64>,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    err_code: u64,
    #[serde(default)]
    description: String,
}

impl From<ApiErrorBody> for BatchRejection {
    fn from(body: ApiErrorBody) -> Self {
        Self {
            code: ErrorCode(body.err_code),
            description: body.description,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::RANDOM_ID_BYTES;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn batch() -> AtomicBatch {
        AtomicBatch::with_id(BatchId::from([4u8; RANDOM_ID_BYTES]))
    }

    fn record(subject: &str, payload: usize) -> BatchRecord {
        BatchRecord::put(
            Subject::from(subject),
            Bytes::from(vec![b'x'; payload]),
            Expected::Empty,
        )
    }

    #[test]
    fn rejects_a_duplicate_subject() -> TestResult {
        let mut batch = batch();
        batch.push(record("a.b", 1))?;
        assert_eq!(
            batch.push(record("a.b", 2)),
            Err(BatchError::DuplicateSubject(Subject::from("a.b")))
        );
        assert_eq!(batch.len(), 1);
        Ok(())
    }

    #[test]
    fn rejects_a_sixty_seventh_record() -> TestResult {
        let mut batch = batch();
        for index in 0..BATCH_MAX_MESSAGES {
            batch.push(record(&format!("a.{index}"), 1))?;
        }
        assert_eq!(batch.push(record("a.overflow", 1)), Err(BatchError::TooManyRecords));
        Ok(())
    }

    #[test]
    fn rejects_one_byte_past_one_mebibyte_including_headers() -> TestResult {
        let mut probe = batch();
        probe.push(record("a.big", 0))?;
        let room = BATCH_MAX_BYTES - probe.encoded_len();

        let mut exact = batch();
        exact.push(record("a.big", room))?;
        assert_eq!(exact.encoded_len(), BATCH_MAX_BYTES);

        let mut over = batch();
        assert_eq!(
            over.push(record("a.big", room + 1)),
            Err(BatchError::TooLarge {
                encoded: BATCH_MAX_BYTES + 1
            })
        );
        assert!(over.is_empty());
        Ok(())
    }

    #[test]
    fn only_the_last_record_carries_the_commit_header() -> TestResult {
        let mut batch = batch();
        for index in 0..3 {
            batch.push(record(&format!("a.{index}"), 1))?;
        }
        let outbound = batch.outbound(&JetStreamRoute::local());
        let commits: Vec<Option<&str>> = outbound
            .iter()
            .map(|message| message.headers.get(NATS_BATCH_COMMIT).map(|value| value.as_str()))
            .collect();
        assert_eq!(commits, vec![None, None, Some("1")]);
        let sequences: Vec<&str> = outbound
            .iter()
            .filter_map(|message| message.headers.get(NATS_BATCH_SEQUENCE).map(|value| value.as_str()))
            .collect();
        assert_eq!(sequences, vec!["1", "2", "3"]);
        let id = batch.id().to_string();
        assert!(outbound
            .iter()
            .all(|message| message.headers.get(NATS_BATCH_ID).map(|value| value.as_str()) == Some(id.as_str())));
        Ok(())
    }

    #[test]
    fn records_keep_their_ttl_cas_and_operation_headers() -> TestResult {
        let mut batch = batch();
        let ttl = MessageTtl::try_from(Duration::from_secs(15))?;
        batch.push(
            BatchRecord::purge(
                Subject::from("a.p"),
                Bytes::from_static(b"{}"),
                Expected::At(EntryRevision::from(9)),
            )
            .with_ttl(ttl),
        )?;
        let headers = &batch.outbound(&JetStreamRoute::local())[0].headers;
        let get = |name| headers.get(name).map(|value| value.as_str().to_owned());
        assert_eq!(get(NATS_MESSAGE_TTL).as_deref(), Some("15s"));
        assert_eq!(get(NATS_EXPECTED_LAST_SUBJECT_SEQUENCE).as_deref(), Some("9"));
        assert_eq!(get(NATS_ROLLUP).as_deref(), Some("sub"));
        assert_eq!(
            headers.get(KV_OPERATION_HEADER).map(|value| value.as_str()),
            Some("PURGE")
        );
        Ok(())
    }

    #[test]
    fn revision_of_stays_exact_at_u64_boundaries() {
        let last = BatchPosition(66);
        let top = BatchAck {
            final_sequence: EntryRevision::from(u64::MAX),
            last,
        };
        assert_eq!(top.revision_of(last), Ok(EntryRevision::from(u64::MAX)));
        assert_eq!(
            top.revision_of(BatchPosition::FIRST),
            Ok(EntryRevision::from(u64::MAX - 65))
        );
        assert!(matches!(
            top.revision_of(BatchPosition(67)),
            Err(BatchRevisionError::OutOfRange { position: 67, last: 66 })
        ));

        let bottom = BatchAck {
            final_sequence: EntryRevision::from(66),
            last,
        };
        assert_eq!(bottom.revision_of(BatchPosition::FIRST), Ok(EntryRevision::from(1)));
        let underflow = BatchAck {
            final_sequence: EntryRevision::from(64),
            last,
        };
        assert!(matches!(
            underflow.revision_of(BatchPosition::FIRST),
            Err(BatchRevisionError::Overflow(_))
        ));
    }

    fn rejection(code: u64, description: &str) -> BatchRejection {
        BatchRejection {
            code: ErrorCode(code),
            description: description.to_owned(),
        }
    }

    #[test]
    fn only_the_two_inflight_pressure_descriptions_are_retried() {
        assert!(rejection(10211, "atomic publish too many inflight").is_inflight_pressure());
        assert!(rejection(10176, "atomic publish batch is incomplete").is_inflight_pressure());
        for other in [
            "wrong last sequence: 4",
            "atomic publish batch is incomplete, try later",
            "Atomic publish too many inflight",
            "atomic publish is disabled",
            "atomic publish batch is too large",
            "",
        ] {
            assert!(!rejection(10071, other).is_inflight_pressure(), "{other}");
        }
    }

    #[test]
    fn inflight_batch_limit_is_bounded_and_parsed() -> TestResult {
        assert_eq!(InflightBatchLimit::default().get(), DEFAULT_INFLIGHT_BATCH_LIMIT);
        assert_eq!(InflightBatchLimit::try_from(1)?.get(), 1);
        assert_eq!(
            InflightBatchLimit::try_from(MAX_INFLIGHT_BATCH_LIMIT)?.get(),
            MAX_INFLIGHT_BATCH_LIMIT
        );
        assert_eq!(
            InflightBatchLimit::try_from(0),
            Err(InflightBatchLimitError::OutOfRange(0))
        );
        assert!(InflightBatchLimit::try_from(MAX_INFLIGHT_BATCH_LIMIT + 1).is_err());
        assert_eq!(" 8 ".parse::<InflightBatchLimit>()?.get(), 8);
        assert!(matches!(
            "eight".parse::<InflightBatchLimit>(),
            Err(InflightBatchLimitError::NotANumber(_))
        ));
        assert!("0".parse::<InflightBatchLimit>().is_err());
        assert_eq!(
            InflightBatchLimit::default()
                .to_string()
                .parse::<InflightBatchLimit>()?,
            InflightBatchLimit::default()
        );
        Ok(())
    }

    #[tokio::test]
    async fn inflight_gate_clones_share_permits() -> TestResult {
        let gate = InflightBatches::new(InflightBatchLimit::try_from(2)?);
        let shared = gate.clone();
        let first = gate.admit().await;
        let second = shared.admit().await;
        assert!(first.is_some() && second.is_some());
        assert!(tokio::time::timeout(Duration::from_millis(20), gate.admit())
            .await
            .is_err());
        drop(first);
        assert!(tokio::time::timeout(Duration::from_millis(200), shared.admit())
            .await?
            .is_some());
        assert_eq!(
            InflightBatches::process_default().limit(),
            InflightBatchLimit::default()
        );
        Ok(())
    }

    #[test]
    fn pressure_backoff_stays_under_its_ceiling() {
        for retry in 0..40 {
            let ceiling = PRESSURE_BACKOFF_BASE
                .saturating_mul(1u32.checked_shl(retry).unwrap_or(u32::MAX))
                .min(PRESSURE_BACKOFF_CAP);
            assert!(pressure_backoff(retry) <= ceiling, "retry {retry}");
        }
    }

    #[test]
    fn budget_requires_send_within_total() {
        assert!(BatchBudget::new(Duration::from_secs(2), Duration::from_secs(1)).is_err());
        assert!(BatchBudget::new(Duration::ZERO, Duration::from_secs(1)).is_err());
        assert_eq!(BatchBudget::default().send(), BATCH_SEND_BUDGET);
    }
}
