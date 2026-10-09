use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::consumer::pull::{self, MessagesError};
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, PullConsumer, ReplayPolicy, StreamError};
use async_nats::jetstream::context::ConsumerInfoError;
use async_nats::jetstream::stream::{ConsumerError, RawMessageError, RawMessageErrorKind, Stream};
use async_nats::jetstream::Message;
use async_nats::{HeaderMap, Subject};
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::time::Instant;

use crate::entropy::{encode_id, random_id_bytes, EntropyError};
use crate::revision::Revision;

const REPLAY_MAX_MESSAGES: usize = 256;
const REPLAY_MAX_BYTES: usize = 1024 * 1024;
const REPLAY_PULL_EXPIRY: Duration = Duration::from_secs(1);
const REPLAY_INACTIVE_THRESHOLD: Duration = Duration::from_secs(30);
const REPLAY_NAME_PREFIX: &str = "presence_replay_";
const DEFAULT_REBUILD_BUDGET: Duration = Duration::from_secs(5);
const REPLAY_OPEN_ATTEMPT: Duration = Duration::from_secs(1);
const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConsumerSequence(u64);

impl ConsumerSequence {
    pub fn get(self) -> u64 {
        self.0
    }

    fn follows(self, previous: Self) -> bool {
        previous.0.checked_add(1) == Some(self.0)
    }
}

impl From<u64> for ConsumerSequence {
    fn from(sequence: u64) -> Self {
        Self(sequence)
    }
}

impl fmt::Display for ConsumerSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingCount(u64);

impl PendingCount {
    pub fn get(self) -> u64 {
        self.0
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl From<u64> for PendingCount {
    fn from(pending: u64) -> Self {
        Self(pending)
    }
}

impl fmt::Display for PendingCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamTimestamp(SystemTime);

impl StreamTimestamp {
    pub fn get(self) -> SystemTime {
        self.0
    }

    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration).map(Self)
    }

    fn from_unix_nanos(nanos: i128) -> Self {
        let magnitude = Duration::from_nanos(u64::try_from(nanos.unsigned_abs()).unwrap_or(u64::MAX));
        let time = if nanos >= 0 {
            UNIX_EPOCH.checked_add(magnitude)
        } else {
            UNIX_EPOCH.checked_sub(magnitude)
        };
        Self(time.unwrap_or(UNIX_EPOCH))
    }
}

impl From<SystemTime> for StreamTimestamp {
    fn from(time: SystemTime) -> Self {
        Self(time)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RebuildBudget(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rebuild budget must be greater than zero")]
pub struct RebuildBudgetError;

impl RebuildBudget {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for RebuildBudget {
    fn default() -> Self {
        Self(DEFAULT_REBUILD_BUDGET)
    }
}

impl TryFrom<Duration> for RebuildBudget {
    type Error = RebuildBudgetError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(RebuildBudgetError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReconcileInterval(Duration);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("reconcile interval must be greater than zero")]
pub struct ReconcileIntervalError;

impl ReconcileInterval {
    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for ReconcileInterval {
    fn default() -> Self {
        Self(DEFAULT_RECONCILE_INTERVAL)
    }
}

impl TryFrom<Duration> for ReconcileInterval {
    type Error = ReconcileIntervalError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        if value.is_zero() {
            Err(ReconcileIntervalError)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplayFilters(Vec<String>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a replay consumer needs at least one filter subject")]
pub struct ReplayFiltersError;

impl ReplayFilters {
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl TryFrom<Vec<String>> for ReplayFilters {
    type Error = ReplayFiltersError;

    fn try_from(filters: Vec<String>) -> Result<Self, Self::Error> {
        if filters.is_empty() {
            Err(ReplayFiltersError)
        } else {
            Ok(Self(filters))
        }
    }
}

impl From<String> for ReplayFilters {
    fn from(filter: String) -> Self {
        Self(vec![filter])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplayConsumerName(String);

impl ReplayConsumerName {
    pub fn generate() -> Result<Self, EntropyError> {
        Ok(Self(format!("{REPLAY_NAME_PREFIX}{}", encode_id(&random_id_bytes()?))))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReplayConsumerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct RawRecord {
    subject: Subject,
    headers: Option<HeaderMap>,
    payload: Bytes,
    stream_seq: Revision,
    consumer_seq: ConsumerSequence,
    published: StreamTimestamp,
    pending: PendingCount,
}

#[derive(Debug, Clone)]
pub struct RecordPosition {
    pub stream_seq: Revision,
    pub consumer_seq: ConsumerSequence,
    pub published: StreamTimestamp,
    pub pending: PendingCount,
}

impl RawRecord {
    pub fn new(subject: Subject, headers: Option<HeaderMap>, payload: Bytes, position: RecordPosition) -> Self {
        Self {
            subject,
            headers,
            payload,
            stream_seq: position.stream_seq,
            consumer_seq: position.consumer_seq,
            published: position.published,
            pending: position.pending,
        }
    }

    pub fn subject(&self) -> &str {
        self.subject.as_str()
    }

    pub fn headers(&self) -> Option<&HeaderMap> {
        self.headers.as_ref()
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn stream_seq(&self) -> Revision {
        self.stream_seq
    }

    pub fn consumer_seq(&self) -> ConsumerSequence {
        self.consumer_seq
    }

    pub fn published(&self) -> StreamTimestamp {
        self.published
    }

    pub fn pending(&self) -> PendingCount {
        self.pending
    }
}

impl TryFrom<Message> for RawRecord {
    type Error = ReplayError;

    fn try_from(message: Message) -> Result<Self, Self::Error> {
        let position = {
            let info = message.info().map_err(|err| ReplayError::Metadata(err.to_string()))?;
            RecordPosition {
                stream_seq: Revision::from(info.stream_sequence),
                consumer_seq: ConsumerSequence::from(info.consumer_sequence),
                published: StreamTimestamp::from_unix_nanos(info.published.unix_timestamp_nanos()),
                pending: PendingCount::from(info.pending),
            }
        };
        let message = message.message;
        Ok(Self::new(message.subject, message.headers, message.payload, position))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watermark {
    CaughtUp {
        applied: ConsumerSequence,
    },
    Lagging {
        pending: PendingCount,
        delivered: ConsumerSequence,
        applied: ConsumerSequence,
    },
    Behind {
        stored: Revision,
        applied: ConsumerSequence,
    },
    Unknown,
}

impl Watermark {
    pub fn is_caught_up(self) -> bool {
        matches!(self, Self::CaughtUp { .. })
    }

    fn observe(pending: PendingCount, delivered: ConsumerSequence, applied: ConsumerSequence) -> Self {
        if pending.is_zero() && delivered == applied {
            Self::CaughtUp { applied }
        } else {
            Self::Lagging {
                pending,
                delivered,
                applied,
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("replay did not reach zero pending within {budget:?}: applied {applied}, pending {pending}")]
    NotReady {
        budget: Duration,
        applied: ConsumerSequence,
        pending: PendingCount,
    },
    #[error("could not name the replay consumer: {0}")]
    Entropy(#[from] EntropyError),
    #[error("could not create the replay consumer: {0}")]
    Consumer(#[from] ConsumerError),
    #[error("could not start pulling from the replay consumer: {0}")]
    Subscribe(#[from] StreamError),
    #[error("replay pull failed: {0}")]
    Pull(#[from] MessagesError),
    #[error("replay consumer info failed: {0}")]
    Info(#[from] ConsumerInfoError),
    #[error("replay message has no stream metadata: {0}")]
    Metadata(String),
    #[error("replay stream ended")]
    Ended,
    #[error("replay delivery gap: expected consumer sequence after {applied}, got {found}")]
    Gap {
        applied: ConsumerSequence,
        found: ConsumerSequence,
    },
}

impl ReplayError {
    pub fn is_not_ready(&self) -> bool {
        matches!(self, Self::NotReady { .. })
    }
}

pub struct ReplayConsumer {
    stream: Stream,
    filters: ReplayFilters,
    consumer: PullConsumer,
    name: ReplayConsumerName,
    messages: pull::Stream,
    applied: ConsumerSequence,
    pending: PendingCount,
}

impl ReplayConsumer {
    pub async fn rebuild<F: FnMut(RawRecord)>(
        stream: &Stream,
        filters: &ReplayFilters,
        budget: RebuildBudget,
        apply: F,
    ) -> Result<Self, ReplayError> {
        let deadline = Instant::now() + budget.get();
        let mut replay = loop {
            let name = ReplayConsumerName::generate()?;
            let attempt = (Instant::now() + REPLAY_OPEN_ATTEMPT).min(deadline);
            match tokio::time::timeout_at(attempt, Self::open(stream, filters, name.clone())).await {
                Ok(opened) => break opened?,
                Err(_) => {
                    spawn_delete(stream.clone(), name);
                    if Instant::now() >= deadline {
                        return Err(ReplayError::NotReady {
                            budget: budget.get(),
                            applied: ConsumerSequence::default(),
                            pending: PendingCount::default(),
                        });
                    }
                    tracing::warn!(
                        stream = %stream.cached_info().config.name,
                        "replay consumer creation outlived its attempt bound, retrying with a fresh consumer"
                    );
                }
            }
        };
        match tokio::time::timeout_at(deadline, replay.catch_up(apply)).await {
            Ok(Ok(())) => Ok(replay),
            Ok(Err(err)) => {
                replay.discard();
                Err(err)
            }
            Err(_) => {
                let (applied, pending) = (replay.applied, replay.pending);
                replay.discard();
                Err(ReplayError::NotReady {
                    budget: budget.get(),
                    applied,
                    pending,
                })
            }
        }
    }

    async fn open(stream: &Stream, filters: &ReplayFilters, name: ReplayConsumerName) -> Result<Self, ReplayError> {
        let consumer: PullConsumer = stream
            .create_consumer(pull::Config {
                name: Some(name.as_str().to_owned()),
                deliver_policy: DeliverPolicy::LastPerSubject,
                ack_policy: AckPolicy::None,
                replay_policy: ReplayPolicy::Instant,
                filter_subjects: filters.as_slice().to_vec(),
                memory_storage: true,
                num_replicas: 1,
                inactive_threshold: REPLAY_INACTIVE_THRESHOLD,
                ..Default::default()
            })
            .await?;
        let pending = PendingCount::from(consumer.cached_info().num_pending);
        let messages = consumer
            .stream()
            .max_messages_per_batch(REPLAY_MAX_MESSAGES)
            .max_bytes_per_batch(REPLAY_MAX_BYTES)
            .expires(REPLAY_PULL_EXPIRY)
            .messages()
            .await?;
        Ok(Self {
            stream: stream.clone(),
            filters: filters.clone(),
            consumer,
            name,
            messages,
            applied: ConsumerSequence::default(),
            pending,
        })
    }

    async fn catch_up<F: FnMut(RawRecord)>(&mut self, mut apply: F) -> Result<(), ReplayError> {
        while !self.pending.is_zero() {
            let record = self.next().await?;
            apply(record);
        }
        Ok(())
    }

    pub async fn next(&mut self) -> Result<RawRecord, ReplayError> {
        let message = self.messages.next().await.ok_or(ReplayError::Ended)??;
        let record = RawRecord::try_from(message)?;
        if !record.consumer_seq.follows(self.applied) {
            return Err(ReplayError::Gap {
                applied: self.applied,
                found: record.consumer_seq,
            });
        }
        self.applied = record.consumer_seq;
        self.pending = record.pending;
        Ok(record)
    }

    pub async fn watermark(&self) -> Watermark {
        let info = match self.consumer.get_info().await {
            Ok(info) => info,
            Err(err) => {
                tracing::debug!(%err, "replay watermark unavailable");
                return Watermark::Unknown;
            }
        };
        let observed = Watermark::observe(
            PendingCount::from(info.num_pending),
            ConsumerSequence::from(info.delivered.consumer_sequence),
            self.applied,
        );
        if !observed.is_caught_up() {
            return observed;
        }
        match self.stored_after(Revision::from(info.delivered.stream_sequence)).await {
            Ok(None) => observed,
            Ok(Some(stored)) => Watermark::Behind {
                stored,
                applied: self.applied,
            },
            Err(err) => {
                tracing::debug!(%err, "replay watermark could not read the stream past the consumer");
                Watermark::Unknown
            }
        }
    }

    async fn stored_after(&self, delivered: Revision) -> Result<Option<Revision>, RawMessageError> {
        let Ok(from) = delivered.checked_add(1) else {
            return Ok(None);
        };
        for filter in self.filters.as_slice() {
            let found = self
                .stream
                .raw_message_builder()
                .sequence(from.get())
                .next_by_subject(filter.as_str())
                .send()
                .await;
            match found {
                Ok(message) => return Ok(Some(Revision::from(message.sequence))),
                Err(err) if err.kind() == RawMessageErrorKind::NoMessageFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }

    pub fn name(&self) -> &ReplayConsumerName {
        &self.name
    }

    pub fn applied(&self) -> ConsumerSequence {
        self.applied
    }

    pub fn discard(self) {
        spawn_delete(self.stream, self.name);
    }
}

fn spawn_delete(stream: Stream, name: ReplayConsumerName) {
    tokio::spawn(async move {
        if let Err(err) = stream.delete_consumer(name.as_str()).await {
            tracing::debug!(%err, %name, "could not delete a discarded replay consumer");
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBackoff(Duration);

impl RetryBackoff {
    pub fn get(self) -> Duration {
        self.0
    }

    pub fn jittered(self) -> Duration {
        let ceiling = u64::try_from(self.0.as_millis()).unwrap_or(u64::MAX).max(1);
        let jitter = getrandom::u64().map_or(0, |random| random % ceiling);
        self.0 + Duration::from_millis(jitter)
    }

    pub fn next(self) -> Self {
        Self((self.0 * 2).min(RETRY_BACKOFF_MAX))
    }
}

impl Default for RetryBackoff {
    fn default() -> Self {
        Self(RETRY_BACKOFF_MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_contract() {
        assert_eq!(RebuildBudget::default().get(), Duration::from_secs(5));
        assert_eq!(ReconcileInterval::default().get(), Duration::from_secs(30));
        assert!(RebuildBudget::try_from(Duration::ZERO).is_err());
        assert!(ReconcileInterval::try_from(Duration::ZERO).is_err());
        assert!(ReplayFilters::try_from(Vec::new()).is_err());
    }

    #[test]
    fn watermark_needs_zero_pending_and_everything_applied() {
        let seq = ConsumerSequence::from;
        let pending = PendingCount::from;
        assert!(Watermark::observe(pending(0), seq(4), seq(4)).is_caught_up());
        assert!(!Watermark::observe(pending(1), seq(4), seq(4)).is_caught_up());
        assert!(!Watermark::observe(pending(0), seq(5), seq(4)).is_caught_up());
        assert!(!Watermark::Unknown.is_caught_up());
    }

    #[test]
    fn consumer_sequences_must_be_contiguous() {
        let seq = ConsumerSequence::from;
        assert!(seq(1).follows(seq(0)));
        assert!(!seq(3).follows(seq(1)));
        assert!(!seq(1).follows(seq(1)));
    }

    #[test]
    fn stream_timestamps_convert_from_unix_nanos() {
        let stamp = StreamTimestamp::from_unix_nanos(1_500_000_000);
        assert_eq!(stamp.get(), UNIX_EPOCH + Duration::from_millis(1_500));
    }

    #[test]
    fn retry_backoff_is_bounded_and_jittered() {
        let mut backoff = RetryBackoff::default();
        for _ in 0..10 {
            let delay = backoff.jittered();
            assert!(delay >= backoff.get() && delay < backoff.get() * 2 + Duration::from_millis(1));
            backoff = backoff.next();
        }
        assert_eq!(backoff.get(), RETRY_BACKOFF_MAX);
    }
}
