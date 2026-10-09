use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, SystemTime};

use async_nats::header::{NATS_MARKER_REASON, NATS_MESSAGE_TTL};
use async_nats::HeaderMap;

use crate::config::{BucketName, PresenceConfig};
use crate::constants::{
    KV_OPERATION_DELETE, KV_OPERATION_HEADER, KV_OPERATION_PURGE, KV_SUBJECT_PREFIX, TOKEN_SEPARATOR,
};
use crate::holder::HolderId;
use crate::key::PresenceKey;
use crate::kv_key::{EntryKey, KvKey, KvKeyError};
use crate::meta::Meta;
use crate::phx_ref::{StoredRef, ViewRef};
use crate::revision::Revision;
use crate::shard::{ShardCount, ViewShard};
use crate::topic::Topic;
use crate::value::{StoredValue, ValueError};

use super::presences::{Diff, MetaEntry, Presences};
use super::replay::{RawRecord, ReplayFilters, StreamTimestamp, Watermark};

const TTL_NEVER: &str = "never";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slot {
    key: PresenceKey,
    holder: HolderId,
}

impl Slot {
    pub fn new(key: PresenceKey, holder: HolderId) -> Self {
        Self { key, holder }
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn holder(&self) -> &HolderId {
        &self.holder
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectiveTtl {
    Expires(Duration),
    Never,
    Unstated,
}

impl EffectiveTtl {
    pub fn from_headers(headers: Option<&HeaderMap>) -> Self {
        headers
            .and_then(|headers| headers.get(NATS_MESSAGE_TTL))
            .map_or(Self::Unstated, |value| Self::parse(value.as_str()))
    }

    fn parse(value: &str) -> Self {
        let value = value.trim();
        if value.eq_ignore_ascii_case(TTL_NEVER) {
            return Self::Never;
        }
        if let Ok(seconds) = value.parse::<u64>() {
            return Self::Expires(Duration::from_secs(seconds));
        }
        parse_go_duration(value).map_or(Self::Unstated, Self::Expires)
    }
}

fn parse_go_duration(value: &str) -> Option<Duration> {
    let mut total = Duration::ZERO;
    let mut rest = value;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let amount: u64 = rest.get(..digits)?.parse().ok()?;
        rest = rest.get(digits..)?;
        let unit_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let unit = match rest.get(..unit_len)? {
            "h" => Duration::from_secs(3600),
            "m" => Duration::from_secs(60),
            "s" => Duration::from_secs(1),
            "ms" => Duration::from_millis(1),
            _ => return None,
        };
        rest = rest.get(unit_len..)?;
        total = total.checked_add(unit.checked_mul(u32::try_from(amount).ok()?)?)?;
    }
    Some(total)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryClock {
    published: StreamTimestamp,
    ttl: EffectiveTtl,
}

impl EntryClock {
    pub fn new(published: StreamTimestamp, ttl: EffectiveTtl) -> Self {
        Self { published, ttl }
    }

    pub fn published(self) -> StreamTimestamp {
        self.published
    }

    pub fn ttl(self) -> EffectiveTtl {
        self.ttl
    }

    fn stale_at(self, policy: StalePolicy) -> Option<SystemTime> {
        let ttl = match self.ttl {
            EffectiveTtl::Expires(ttl) => ttl,
            EffectiveTtl::Never => return None,
            EffectiveTtl::Unstated => policy.fallback_ttl,
        };
        self.published
            .checked_add(ttl.saturating_add(policy.grace))
            .map(StreamTimestamp::get)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalePolicy {
    fallback_ttl: Duration,
    grace: Duration,
}

impl StalePolicy {
    pub fn new(fallback_ttl: Duration, grace: Duration) -> Self {
        Self { fallback_ttl, grace }
    }
}

impl From<&PresenceConfig> for StalePolicy {
    fn from(config: &PresenceConfig) -> Self {
        Self::new(config.lease_ttl().get(), config.marker_ttl().get())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RetirementWindow(Duration);

impl RetirementWindow {
    pub fn new(window: Duration) -> Self {
        Self(window)
    }

    pub fn get(self) -> Duration {
        self.0
    }
}

impl From<&PresenceConfig> for RetirementWindow {
    fn from(config: &PresenceConfig) -> Self {
        Self(config.marker_ttl().get())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepOutcome {
    Suspended(Watermark),
    Swept(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEntry {
    phx_ref: StoredRef,
    meta: Meta,
    birth: Revision,
}

impl StoredEntry {
    pub fn new(phx_ref: StoredRef, meta: Meta, birth: Revision) -> Self {
        Self { phx_ref, meta, birth }
    }

    pub fn of(value: &StoredValue, revision: Revision) -> Self {
        Self::new(
            value.phx_ref().clone(),
            value.meta().clone(),
            value.birth_rev().unwrap_or(revision),
        )
    }

    pub fn phx_ref(&self) -> &StoredRef {
        &self.phx_ref
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn birth(&self) -> Revision {
        self.birth
    }
}

#[derive(Debug, Clone)]
pub enum Observation {
    Put {
        slot: Slot,
        revision: Revision,
        entry: StoredEntry,
        clock: EntryClock,
    },
    Leave {
        slot: Slot,
        revision: Revision,
        at: StreamTimestamp,
    },
}

impl Observation {
    pub fn slot(&self) -> &Slot {
        match self {
            Self::Put { slot, .. } | Self::Leave { slot, .. } => slot,
        }
    }

    pub fn revision(&self) -> Revision {
        match self {
            Self::Put { revision, .. } | Self::Leave { revision, .. } => *revision,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Rejection {
    #[error("subject is outside the bucket")]
    Subject,
    #[error("kv key does not decode: {0}")]
    Key(#[from] KvKeyError),
    #[error("kv key belongs to shard {found}, not shard {expected}")]
    Shard { found: u16, expected: u16 },
    #[error("kv key belongs to another topic")]
    Topic,
    #[error("value does not decode: {0}")]
    Value(#[from] ValueError),
    #[error("value topic or key disagrees with the kv key")]
    Mismatch,
}

#[derive(Debug)]
pub enum Classified<T> {
    Observed(T),
    Rejected { reason: Rejection, evict: Option<T> },
}

impl<T> Classified<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Classified<U> {
        match self {
            Self::Observed(observed) => Classified::Observed(f(observed)),
            Self::Rejected { reason, evict } => Classified::Rejected {
                reason,
                evict: evict.map(f),
            },
        }
    }
}

pub type Routed = (Topic, Observation);

pub struct ShardClassifier {
    prefix: String,
    shards: ShardCount,
    shard: ViewShard,
}

impl ShardClassifier {
    pub fn new(bucket: &BucketName, shards: ShardCount, shard: ViewShard) -> Self {
        Self {
            prefix: format!("{KV_SUBJECT_PREFIX}{TOKEN_SEPARATOR}{bucket}{TOKEN_SEPARATOR}"),
            shards,
            shard,
        }
    }

    pub fn shard(&self) -> ViewShard {
        self.shard
    }

    pub fn filter(&self) -> String {
        format!("{}{}{TOKEN_SEPARATOR}>", self.prefix, self.shards.token(self.shard))
    }

    pub fn filters(&self) -> ReplayFilters {
        ReplayFilters::from(self.filter())
    }

    pub fn topic_filters(&self, topic: &Topic) -> ReplayFilters {
        ReplayFilters::from(format!(
            "{}{}{TOKEN_SEPARATOR}*{TOKEN_SEPARATOR}*{TOKEN_SEPARATOR}{}",
            self.prefix,
            self.shards.token(self.shard),
            topic.tokens()
        ))
    }

    pub fn classify(&self, record: &RawRecord) -> Classified<Routed> {
        let revision = record.stream_seq();
        let entry = match self.entry_key(record.subject()) {
            Ok(entry) => entry,
            Err(reason) => return Classified::Rejected { reason, evict: None },
        };
        let slot = Slot::new(entry.key().clone(), *entry.holder());
        let topic = entry.topic().clone();
        let at = record.published();
        if is_leave(record.headers()) {
            return Classified::Observed((topic, Observation::Leave { slot, revision, at }));
        }
        let evicting = |reason: Rejection, topic: Topic, slot: Slot| Classified::Rejected {
            reason,
            evict: Some((topic, Observation::Leave { slot, revision, at })),
        };
        let value = match StoredValue::from_json_bytes(record.payload()) {
            Ok(value) => value,
            Err(err) => return evicting(err.into(), topic, slot),
        };
        if !agrees(&entry, &value) {
            return evicting(Rejection::Mismatch, topic, slot);
        }
        let entry = StoredEntry::of(&value, revision);
        let clock = EntryClock::new(at, EffectiveTtl::from_headers(record.headers()));
        Classified::Observed((
            topic,
            Observation::Put {
                slot,
                revision,
                entry,
                clock,
            },
        ))
    }

    fn entry_key(&self, subject: &str) -> Result<EntryKey, Rejection> {
        let token = subject.strip_prefix(&self.prefix).ok_or(Rejection::Subject)?;
        let entry = EntryKey::decode(&KvKey::try_from(token.to_owned())?, self.shards)?;
        let found = entry.shard(self.shards);
        if found == self.shard {
            Ok(entry)
        } else {
            Err(Rejection::Shard {
                found: found.shard().index(),
                expected: self.shard.shard().index(),
            })
        }
    }
}

pub struct Classifier {
    shard: ShardClassifier,
    topic: Topic,
}

impl Classifier {
    pub fn new(bucket: &BucketName, shards: ShardCount, topic: Topic) -> Self {
        Self {
            shard: ShardClassifier::new(bucket, shards, ViewShard::of(&topic, shards)),
            topic,
        }
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn filters(&self) -> ReplayFilters {
        self.shard.topic_filters(&self.topic)
    }

    pub fn classify(&self, record: &RawRecord) -> Classified<Observation> {
        match self.shard.classify(record) {
            Classified::Observed((topic, _))
            | Classified::Rejected {
                evict: Some((topic, _)),
                ..
            } if topic != self.topic => Classified::Rejected {
                reason: Rejection::Topic,
                evict: None,
            },
            classified => classified.map(|(_, observation)| observation),
        }
    }
}

pub(crate) fn is_leave(headers: Option<&HeaderMap>) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    headers.get(NATS_MARKER_REASON).is_some()
        || headers
            .get(KV_OPERATION_HEADER)
            .is_some_and(|op| op.as_str() == KV_OPERATION_PURGE || op.as_str() == KV_OPERATION_DELETE)
}

fn agrees(entry: &EntryKey, value: &StoredValue) -> bool {
    value.topic() == entry.topic() && value.key() == entry.key()
}

#[derive(Debug, Clone)]
struct Cached {
    revision: Revision,
    entry: StoredEntry,
    clock: EntryClock,
}

#[derive(Debug, Clone, Copy)]
struct Retired {
    revision: Revision,
    at: StreamTimestamp,
}

#[derive(Debug, Default)]
pub struct LiveSet {
    slots: HashMap<Slot, Cached>,
    retired: HashMap<Slot, Retired>,
    revision: Option<Revision>,
}

impl LiveSet {
    pub fn apply(&mut self, observation: Observation) -> Option<Slot> {
        self.advance(observation.revision());
        match observation {
            Observation::Put {
                slot,
                revision,
                entry,
                clock,
            } => {
                if self
                    .retired
                    .get(&slot)
                    .is_some_and(|retired| revision <= retired.revision)
                {
                    return None;
                }
                match self.slots.get_mut(&slot) {
                    Some(cached) if revision <= cached.revision => None,
                    Some(cached) if cached.entry.phx_ref == entry.phx_ref => {
                        cached.revision = revision;
                        cached.clock = clock;
                        None
                    }
                    _ => {
                        self.retired.remove(&slot);
                        let cached = Cached { revision, entry, clock };
                        self.slots.insert(slot.clone(), cached);
                        Some(slot)
                    }
                }
            }
            Observation::Leave { slot, revision, at } => {
                if self.slots.get(&slot).is_some_and(|cached| revision <= cached.revision) {
                    return None;
                }
                let newer = self
                    .retired
                    .get(&slot)
                    .is_none_or(|retired| revision > retired.revision);
                if newer {
                    self.retired.insert(slot.clone(), Retired { revision, at });
                }
                self.slots.remove(&slot).map(|_| slot)
            }
        }
    }

    pub fn advance(&mut self, revision: Revision) {
        self.revision = self.revision.max(Some(revision));
    }

    pub fn revision(&self) -> Option<Revision> {
        self.revision
    }

    pub fn entry_revision(&self, slot: &Slot) -> Option<Revision> {
        self.slots.get(slot).map(|cached| cached.revision)
    }

    pub fn retirement_revision(&self, slot: &Slot) -> Option<Revision> {
        self.retired.get(slot).map(|retired| retired.revision)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn stale(&self, now: SystemTime, policy: StalePolicy) -> Vec<Slot> {
        self.slots
            .iter()
            .filter(|(_, cached)| cached.clock.stale_at(policy).is_some_and(|at| now > at))
            .map(|(slot, _)| slot.clone())
            .collect()
    }

    fn forget_retired(&mut self, now: SystemTime, window: RetirementWindow) {
        self.retired.retain(|_, retired| {
            retired
                .at
                .checked_add(window.get())
                .is_some_and(|until| now <= until.get())
        });
    }

    fn absorb_retired(&mut self, older: HashMap<Slot, Retired>) {
        for (slot, retired) in older {
            if self
                .slots
                .get(&slot)
                .is_some_and(|cached| cached.revision >= retired.revision)
            {
                continue;
            }
            let keep = self
                .retired
                .get(&slot)
                .is_none_or(|kept| kept.revision < retired.revision);
            if keep {
                self.retired.insert(slot, retired);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FetchEntry {
    key: PresenceKey,
    phx_ref: StoredRef,
    meta: Meta,
}

impl FetchEntry {
    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn stored_ref(&self) -> &StoredRef {
        &self.phx_ref
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn with_meta(self, meta: Meta) -> Self {
        Self { meta, ..self }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("fetch failed: {0}")]
    Failed(String),
    #[error("fetch returned a different set of entries than it was given")]
    IdentityChanged,
}

impl FetchError {
    pub fn failed(reason: impl fmt::Display) -> Self {
        Self::Failed(reason.to_string())
    }
}

#[derive(Debug, Default)]
pub struct RenderBatch {
    slots: Vec<(Slot, Option<StoredEntry>)>,
}

impl RenderBatch {
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn requests(&self) -> Vec<FetchEntry> {
        self.slots
            .iter()
            .filter_map(|(slot, after)| {
                after.as_ref().map(|entry| FetchEntry {
                    key: slot.key.clone(),
                    phx_ref: entry.phx_ref.clone(),
                    meta: entry.meta.clone(),
                })
            })
            .collect()
    }

    fn rendered(&self, fetched: Vec<FetchEntry>) -> Result<HashMap<StoredRef, Meta>, FetchError> {
        let expected: HashMap<&StoredRef, &PresenceKey> = self
            .slots
            .iter()
            .filter_map(|(slot, after)| after.as_ref().map(|entry| (&entry.phx_ref, &slot.key)))
            .collect();
        if fetched.len() != expected.len() {
            return Err(FetchError::IdentityChanged);
        }
        let mut rendered = HashMap::with_capacity(fetched.len());
        for entry in fetched {
            if expected.get(&entry.phx_ref) != Some(&&entry.key) {
                return Err(FetchError::IdentityChanged);
            }
            if rendered.insert(entry.phx_ref, entry.meta).is_some() {
                return Err(FetchError::IdentityChanged);
            }
        }
        Ok(rendered)
    }
}

#[derive(Debug, Clone)]
struct Emitted {
    birth: Revision,
    entry: MetaEntry,
}

fn view_ref(stored: &StoredEntry, rendered: &Meta) -> ViewRef {
    if rendered == &stored.meta {
        ViewRef::from(&stored.phx_ref)
    } else {
        ViewRef::derive(&stored.phx_ref, rendered)
    }
}

#[derive(Debug, Default)]
pub struct WatchCore {
    live: LiveSet,
    emitted: HashMap<Slot, Emitted>,
    dirty: HashSet<Slot>,
}

impl WatchCore {
    pub fn new(live: LiveSet) -> Self {
        let dirty = live.slots.keys().cloned().collect();
        Self {
            live,
            emitted: HashMap::new(),
            dirty,
        }
    }

    pub fn apply(&mut self, observation: Observation) {
        if let Some(slot) = self.live.apply(observation) {
            self.dirty.insert(slot);
        }
    }

    pub fn has_stale(&self, now: SystemTime, policy: StalePolicy) -> bool {
        !self.live.stale(now, policy).is_empty()
    }

    pub fn sweep(&mut self, watermark: Watermark, now: SystemTime, policy: StalePolicy) -> SweepOutcome {
        if !watermark.is_caught_up() {
            return SweepOutcome::Suspended(watermark);
        }
        let stale = self.live.stale(now, policy);
        for slot in &stale {
            self.live.slots.remove(slot);
        }
        let swept = stale.len();
        self.dirty.extend(stale);
        SweepOutcome::Swept(swept)
    }

    pub fn forget_retired(&mut self, now: SystemTime, window: RetirementWindow) {
        self.live.forget_retired(now, window);
    }

    pub fn reconcile(&mut self, mut fresh: LiveSet) {
        self.dirty.extend(self.live.slots.keys().cloned());
        self.dirty.extend(fresh.slots.keys().cloned());
        self.dirty.extend(self.emitted.keys().cloned());
        if let Some(revision) = self.live.revision {
            fresh.advance(revision);
        }
        fresh.absorb_retired(std::mem::take(&mut self.live.retired));
        self.live = fresh;
    }

    pub fn advance(&mut self, revision: Revision) {
        self.live.advance(revision);
    }

    pub fn revision(&self) -> Option<Revision> {
        self.live.revision
    }

    pub fn entry_revision(&self, slot: &Slot) -> Option<Revision> {
        self.live.entry_revision(slot)
    }

    pub fn retirement_revision(&self, slot: &Slot) -> Option<Revision> {
        self.live.retirement_revision(slot)
    }

    pub fn has_pending(&self) -> bool {
        !self.dirty.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.emitted.is_empty()
    }

    pub fn render(&mut self) -> RenderBatch {
        let slots = std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|slot| {
                let after = self.live.slots.get(&slot).map(|cached| cached.entry.clone());
                (slot, after)
            })
            .collect();
        RenderBatch { slots }
    }

    pub fn restore(&mut self, batch: RenderBatch) {
        self.dirty.extend(batch.slots.into_iter().map(|(slot, _)| slot));
    }

    pub fn commit(&mut self, batch: RenderBatch, fetched: Vec<FetchEntry>) -> Result<Diff, (RenderBatch, FetchError)> {
        let rendered = match batch.rendered(fetched) {
            Ok(rendered) => rendered,
            Err(err) => return Err((batch, err)),
        };
        let mut joins = Vec::new();
        let mut leaves = Vec::new();
        for (slot, after) in batch.slots {
            let before = self.emitted.get(&slot);
            let next = after.and_then(|stored| {
                let meta = rendered.get(&stored.phx_ref)?.clone();
                Some((view_ref(&stored, &meta), meta, stored.birth))
            });
            let unchanged = match (before, &next) {
                (Some(before), Some((view, _, _))) => before.entry.phx_ref() == view,
                (None, None) => true,
                _ => false,
            };
            if unchanged {
                continue;
            }
            let previous = before.map(|before| before.entry.phx_ref().clone());
            if let Some(before) = before {
                leaves.push((slot.key.clone(), before.clone()));
            }
            match next {
                Some((view, meta, birth)) => {
                    let emitted = Emitted {
                        birth,
                        entry: MetaEntry::new(view, previous, meta),
                    };
                    joins.push((slot.key.clone(), emitted.clone()));
                    self.emitted.insert(slot, emitted);
                }
                None => {
                    self.emitted.remove(&slot);
                }
            }
        }
        Ok(Diff::new(ordered(joins), ordered(leaves)))
    }

    pub fn flush(&mut self) -> Diff {
        let batch = self.render();
        let identity = batch.requests();
        match self.commit(batch, identity) {
            Ok(diff) => diff,
            Err((batch, _)) => {
                self.restore(batch);
                Diff::default()
            }
        }
    }

    pub fn presences(&self) -> Presences {
        ordered(
            self.emitted
                .iter()
                .map(|(slot, emitted)| (slot.key.clone(), emitted.clone()))
                .collect(),
        )
    }

    pub fn metas_for(&self, key: &PresenceKey) -> Vec<MetaEntry> {
        let mut entries: Vec<&Emitted> = self
            .emitted
            .iter()
            .filter(|(slot, _)| &slot.key == key)
            .map(|(_, emitted)| emitted)
            .collect();
        entries.sort_by(|a, b| birth_order(a, b));
        entries.into_iter().map(|emitted| emitted.entry.clone()).collect()
    }
}

fn birth_order(a: &Emitted, b: &Emitted) -> std::cmp::Ordering {
    a.birth
        .cmp(&b.birth)
        .then_with(|| a.entry.phx_ref().as_str().cmp(b.entry.phx_ref().as_str()))
}

fn ordered(mut entries: Vec<(PresenceKey, Emitted)>) -> Presences {
    entries.sort_by(|(a_key, a), (b_key, b)| a_key.cmp(b_key).then_with(|| birth_order(a, b)));
    entries.into_iter().map(|(key, emitted)| (key, emitted.entry)).collect()
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use async_nats::Subject;
    use bytes::Bytes;

    use super::*;
    use crate::meta::Meta;
    use crate::watch::replay::{ConsumerSequence, PendingCount, RecordPosition};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PREFIX: &str = "$KV.PRESENCE_V1.";
    const HOLDER: &str = "q3V9hX0bS2mWf1ZkR8aT1A";

    fn classifier() -> Result<Classifier, Box<dyn std::error::Error>> {
        Ok(Classifier::new(
            &BucketName::default(),
            ShardCount::DEFAULT,
            "room:lobby".parse()?,
        ))
    }

    fn shard_classifier() -> Result<ShardClassifier, Box<dyn std::error::Error>> {
        let shards = ShardCount::DEFAULT;
        Ok(ShardClassifier::new(
            &BucketName::default(),
            shards,
            ViewShard::from(shards.shard(56)?),
        ))
    }

    fn routed(classified: Classified<Routed>) -> Result<Routed, Box<dyn std::error::Error>> {
        match classified {
            Classified::Observed(routed) => Ok(routed),
            Classified::Rejected { reason, .. } => Err(reason.into()),
        }
    }

    fn value(topic: &str, key: &str, phx_ref: &str, prev: Option<&str>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let prev = serde_json::to_string(&prev)?;
        let json = format!(
            r#"{{"v":2,"topic":{topic:?},"key":{key:?},"phx_ref":{phx_ref:?},"phx_ref_prev":{prev},"meta":{{}},"lifetime":"AAAAAAAAAAAAAAAAAAAAAA","mutation_seq":"1","last_op":{{"id":"AAAAAAAAAAAAAAAAAAAAAA","fingerprint":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","outcome":"tracked"}},"client_meta":{{}},"birth_rev":null}}"#
        );
        Ok(StoredValue::from_json_bytes(json.as_bytes())?.to_json_bytes()?)
    }

    fn at(seconds: u64) -> StreamTimestamp {
        StreamTimestamp::from(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    fn record(subject: &str, headers: Option<&HeaderMap>, payload: &[u8], revision: Revision) -> RawRecord {
        RawRecord::new(
            Subject::from(subject),
            headers.cloned(),
            Bytes::copy_from_slice(payload),
            RecordPosition {
                stream_seq: revision,
                consumer_seq: ConsumerSequence::from(revision.get()),
                published: at(revision.get()),
                pending: PendingCount::default(),
            },
        )
    }

    fn put_at(
        key: &str,
        holder: &str,
        phx_ref: &str,
        revision: u64,
        clock: EntryClock,
    ) -> Result<Observation, Box<dyn std::error::Error>> {
        Ok(Observation::Put {
            slot: Slot::new(key.parse()?, holder.parse()?),
            revision: Revision::from(revision),
            entry: StoredEntry::new(StoredRef::parse(phx_ref)?, Meta::default(), Revision::from(revision)),
            clock,
        })
    }

    fn put(key: &str, holder: &str, phx_ref: &str, revision: u64) -> Result<Observation, Box<dyn std::error::Error>> {
        put_at(
            key,
            holder,
            phx_ref,
            revision,
            EntryClock::new(at(revision), EffectiveTtl::Unstated),
        )
    }

    fn leave(key: &str, holder: &str, revision: u64) -> Result<Observation, Box<dyn std::error::Error>> {
        Ok(Observation::Leave {
            slot: Slot::new(key.parse()?, holder.parse()?),
            revision: Revision::from(revision),
            at: at(revision),
        })
    }

    fn caught_up() -> Watermark {
        Watermark::CaughtUp {
            applied: ConsumerSequence::from(1),
        }
    }

    fn policy() -> StalePolicy {
        StalePolicy::new(Duration::from_secs(30), Duration::from_secs(300))
    }

    fn refs(presences: &Presences, key: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(presences
            .get(&key.parse()?)
            .unwrap_or_default()
            .iter()
            .map(|entry| entry.phx_ref().to_string())
            .collect())
    }

    #[test]
    fn classifies_put_and_markers() -> TestResult {
        let classifier = classifier()?;
        let subject = format!("{PREFIX}s56.ana.{HOLDER}.room.lobby");
        let payload = value("room:lobby", "ana", "Fq1", None)?;
        let observation = classifier.classify(&record(&subject, None, &payload, Revision::from(3)));
        assert!(matches!(observation, Classified::Observed(Observation::Put { .. })));

        for (name, header) in [
            (NATS_MARKER_REASON.as_ref(), "MaxAge"),
            (KV_OPERATION_HEADER, KV_OPERATION_PURGE),
            (KV_OPERATION_HEADER, KV_OPERATION_DELETE),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, header);
            let observation = classifier.classify(&record(&subject, Some(&headers), &[], Revision::from(4)));
            assert!(
                matches!(observation, Classified::Observed(Observation::Leave { .. })),
                "{name}: {header}"
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_wrong_shard_and_mismatched_values() -> TestResult {
        let classifier = classifier()?;
        let wrong_shard = format!("{PREFIX}s01.ana.{HOLDER}.room.lobby");
        let payload = value("room:lobby", "ana", "Fq1", None)?;
        let rejected = classifier.classify(&record(&wrong_shard, None, &payload, Revision::from(1)));
        assert!(matches!(
            rejected,
            Classified::Rejected {
                reason: Rejection::Key(KvKeyError::ShardMismatch { .. }),
                evict: None
            }
        ));

        let subject = format!("{PREFIX}s56.ana.{HOLDER}.room.lobby");
        for payload in [
            value("room:other", "ana", "Fq1", None)?,
            value("room:lobby", "bob", "Fq1", None)?,
        ] {
            let rejected = classifier.classify(&record(&subject, None, &payload, Revision::from(2)));
            assert!(matches!(
                rejected,
                Classified::Rejected {
                    reason: Rejection::Mismatch,
                    evict: Some(Observation::Leave { .. })
                }
            ));
        }
        Ok(())
    }

    #[test]
    fn update_is_leave_plus_join() -> TestResult {
        let mut live = LiveSet::default();
        live.apply(put("ana", HOLDER, "A", 1)?);
        let mut core = WatchCore::new(live);
        core.flush();
        core.apply(put("ana", HOLDER, "A", 2)?);
        assert!(!core.has_pending());
        core.apply(put("ana", HOLDER, "B", 3)?);
        let diff = core.flush();
        assert_eq!(refs(diff.leaves(), "ana")?, ["A"]);
        assert_eq!(refs(diff.joins(), "ana")?, ["B"]);
        assert_eq!(refs(&core.presences(), "ana")?, ["B"]);
        Ok(())
    }

    #[test]
    fn coalesces_by_net_change() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        core.apply(put("ana", HOLDER, "A", 1)?);
        core.apply(put("ana", HOLDER, "B", 2)?);
        core.apply(put("ana", HOLDER, "C", 3)?);
        let diff = core.flush();
        assert_eq!(refs(diff.joins(), "ana")?, ["C"]);
        assert!(diff.leaves().is_empty());

        core.apply(leave("ana", HOLDER, 4)?);
        core.apply(put("ana", HOLDER, "D", 5)?);
        core.apply(leave("ana", HOLDER, 6)?);
        let diff = core.flush();
        assert!(diff.joins().is_empty());
        assert_eq!(refs(diff.leaves(), "ana")?, ["C"]);

        core.apply(put("bob", HOLDER, "E", 7)?);
        core.apply(leave("bob", HOLDER, 8)?);
        assert!(core.flush().is_empty());
        Ok(())
    }

    #[test]
    fn drops_redeliveries_and_unknown_leaves() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        core.apply(put("ana", HOLDER, "A", 5)?);
        core.flush();
        core.apply(put("ana", HOLDER, "B", 4)?);
        core.apply(leave("ana", HOLDER, 5)?);
        core.apply(leave("bob", HOLDER, 9)?);
        assert!(core.flush().is_empty());
        Ok(())
    }

    #[test]
    fn sweeps_by_stream_timestamp_plus_effective_ttl() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        let clock = EntryClock::new(at(100), EffectiveTtl::Expires(Duration::from_secs(3)));
        core.apply(put_at("ana", HOLDER, "A", 1, clock)?);
        core.apply(put_at(
            "bob",
            HOLDER,
            "B",
            2,
            EntryClock::new(at(100), EffectiveTtl::Unstated),
        )?);
        core.flush();
        let policy = StalePolicy::new(Duration::from_secs(30), Duration::from_secs(1));
        assert!(!core.has_stale(at(104).get(), policy));
        assert_eq!(core.sweep(caught_up(), at(104).get(), policy), SweepOutcome::Swept(0));
        assert!(core.has_stale(at(105).get(), policy));
        assert_eq!(core.sweep(caught_up(), at(105).get(), policy), SweepOutcome::Swept(1));
        assert_eq!(refs(core.flush().leaves(), "ana")?, ["A"]);
        assert_eq!(core.sweep(caught_up(), at(132).get(), policy), SweepOutcome::Swept(1));
        assert_eq!(refs(core.flush().leaves(), "bob")?, ["B"]);
        Ok(())
    }

    #[test]
    fn sweep_is_suspended_while_the_watermark_lags() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        core.apply(put("ana", HOLDER, "A", 1)?);
        core.flush();
        let later = at(10_000).get();
        let lagging = Watermark::Lagging {
            pending: PendingCount::from(1),
            delivered: ConsumerSequence::from(2),
            applied: ConsumerSequence::from(1),
        };
        assert_eq!(core.sweep(lagging, later, policy()), SweepOutcome::Suspended(lagging));
        assert_eq!(
            core.sweep(Watermark::Unknown, later, policy()),
            SweepOutcome::Suspended(Watermark::Unknown)
        );
        assert!(core.flush().is_empty());
        assert_eq!(core.sweep(caught_up(), later, policy()), SweepOutcome::Swept(1));
        Ok(())
    }

    #[test]
    fn never_ttl_is_not_swept() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        let clock = EntryClock::new(at(1), EffectiveTtl::Never);
        core.apply(put_at("ana", HOLDER, "A", 1, clock)?);
        core.flush();
        assert!(!core.has_stale(at(1_000_000).get(), policy()));
        Ok(())
    }

    #[test]
    fn tombstone_blocks_older_puts_until_retired() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        let slot = Slot::new("ana".parse()?, HOLDER.parse()?);
        core.apply(leave("ana", HOLDER, 9)?);
        assert_eq!(core.retirement_revision(&slot), Some(Revision::from(9)));
        core.apply(put("ana", HOLDER, "A", 8)?);
        assert!(core.flush().is_empty());
        core.apply(put("ana", HOLDER, "B", 10)?);
        assert_eq!(refs(core.flush().joins(), "ana")?, ["B"]);
        assert_eq!(core.retirement_revision(&slot), None);
        assert_eq!(core.entry_revision(&slot), Some(Revision::from(10)));

        core.apply(leave("ana", HOLDER, 11)?);
        core.forget_retired(at(20).get(), RetirementWindow::new(Duration::from_secs(5)));
        assert_eq!(core.retirement_revision(&slot), None);
        Ok(())
    }

    #[test]
    fn reconcile_keeps_newer_tombstones() -> TestResult {
        let mut core = WatchCore::new(LiveSet::default());
        let slot = Slot::new("ana".parse()?, HOLDER.parse()?);
        core.apply(leave("ana", HOLDER, 9)?);
        let mut fresh = LiveSet::default();
        fresh.apply(put("ana", HOLDER, "A", 5)?);
        core.reconcile(fresh);
        assert_eq!(core.retirement_revision(&slot), Some(Revision::from(9)));

        let mut fresh = LiveSet::default();
        fresh.apply(put("ana", HOLDER, "B", 12)?);
        core.reconcile(fresh);
        assert_eq!(core.retirement_revision(&slot), None);
        assert_eq!(core.entry_revision(&slot), Some(Revision::from(12)));
        Ok(())
    }

    #[test]
    fn effective_ttl_reads_the_message_ttl_header() {
        let ttl = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(NATS_MESSAGE_TTL, value);
            EffectiveTtl::from_headers(Some(&headers))
        };
        assert_eq!(ttl("30s"), EffectiveTtl::Expires(Duration::from_secs(30)));
        assert_eq!(ttl("45"), EffectiveTtl::Expires(Duration::from_secs(45)));
        assert_eq!(ttl("1m30s"), EffectiveTtl::Expires(Duration::from_secs(90)));
        assert_eq!(ttl("never"), EffectiveTtl::Never);
        assert_eq!(ttl("soon"), EffectiveTtl::Unstated);
        assert_eq!(EffectiveTtl::from_headers(None), EffectiveTtl::Unstated);
    }

    #[test]
    fn classifier_reads_clock_from_the_record() -> TestResult {
        let classifier = classifier()?;
        let subject = format!("{PREFIX}s56.ana.{HOLDER}.room.lobby");
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_TTL, "3s");
        let payload = value("room:lobby", "ana", "Fq1", None)?;
        let Classified::Observed(Observation::Put { clock, .. }) =
            classifier.classify(&record(&subject, Some(&headers), &payload, Revision::from(7)))
        else {
            panic!("expected a put");
        };
        assert_eq!(clock.published(), at(7));
        assert_eq!(clock.ttl(), EffectiveTtl::Expires(Duration::from_secs(3)));
        Ok(())
    }

    #[test]
    fn reconcile_emits_net_change_against_emitted_view() -> TestResult {
        let other = "AAAAAAAAAAAAAAAAAAAAAA";
        let mut live = LiveSet::default();
        live.apply(put("ana", HOLDER, "A", 1)?);
        live.apply(put("bob", HOLDER, "B", 2)?);
        let mut core = WatchCore::new(live);
        core.flush();

        let mut fresh = LiveSet::default();
        fresh.apply(put("ana", HOLDER, "A", 7)?);
        fresh.apply(put("ana", other, "C", 8)?);
        core.reconcile(fresh);
        let diff = core.flush();
        assert_eq!(refs(diff.joins(), "ana")?, ["C"]);
        assert_eq!(refs(diff.leaves(), "bob")?, ["B"]);
        assert!(diff.leaves().get(&"ana".parse()?).is_none());
        assert_eq!(refs(&core.presences(), "ana")?, ["A", "C"]);
        Ok(())
    }

    #[test]
    fn shard_classifier_routes_topics_separately() -> TestResult {
        let classifier = shard_classifier()?;
        assert_eq!(classifier.filter(), "$KV.PRESENCE_V1.s56.>");
        let lobby = format!("{PREFIX}s56.ana.{HOLDER}.room.lobby");
        let other = format!("{PREFIX}s56.ana.{HOLDER}.room.r37");
        let (lobby_topic, lobby_put) = routed(classifier.classify(&record(
            &lobby,
            None,
            &value("room:lobby", "ana", "Fq1", None)?,
            Revision::from(1),
        )))?;
        let (other_topic, other_put) = routed(classifier.classify(&record(
            &other,
            None,
            &value("room:r37", "ana", "Fq2", None)?,
            Revision::from(2),
        )))?;
        assert_eq!(lobby_topic.as_str(), "room:lobby");
        assert_eq!(other_topic.as_str(), "room:r37");

        let mut cores: HashMap<Topic, WatchCore> = HashMap::new();
        cores.entry(lobby_topic.clone()).or_default().apply(lobby_put);
        cores.entry(other_topic.clone()).or_default().apply(other_put);
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MARKER_REASON, "MaxAge");
        let (topic, leave) = routed(classifier.classify(&record(&other, Some(&headers), &[], Revision::from(3))))?;
        assert!(matches!(leave, Observation::Leave { .. }));
        cores.entry(topic).or_default().apply(leave);

        let lobby_core = cores.get_mut(&lobby_topic).ok_or("no lobby core")?;
        assert_eq!(refs(lobby_core.flush().joins(), "ana")?, ["Fq1"]);
        assert_eq!(lobby_core.revision(), Some(Revision::from(1)));
        let other_core = cores.get_mut(&other_topic).ok_or("no other core")?;
        assert!(other_core.flush().is_empty());
        assert_eq!(other_core.revision(), Some(Revision::from(3)));
        Ok(())
    }

    #[test]
    fn shard_classifier_rejects_mismatch_and_reports_the_slot() -> TestResult {
        let classifier = shard_classifier()?;
        let subject = format!("{PREFIX}s56.ana.{HOLDER}.room.lobby");
        let rejected = classifier.classify(&record(
            &subject,
            None,
            &value("room:lobby", "bob", "Fq1", None)?,
            Revision::from(4),
        ));
        let Classified::Rejected {
            reason: Rejection::Mismatch,
            evict: Some((topic, Observation::Leave { slot, revision, .. })),
        } = rejected
        else {
            panic!("expected a mismatch with an eviction, got {rejected:?}");
        };
        assert_eq!(topic.as_str(), "room:lobby");
        assert_eq!(slot, Slot::new("ana".parse()?, HOLDER.parse()?));
        assert_eq!(revision, Revision::from(4));
        Ok(())
    }

    #[test]
    fn shard_classifier_rejects_wrong_shard_keys() -> TestResult {
        let classifier = shard_classifier()?;
        let other_shard = format!("{PREFIX}s18.ana.{HOLDER}.room.r0");
        let rejected = classifier.classify(&record(
            &other_shard,
            None,
            &value("room:r0", "ana", "Fq1", None)?,
            Revision::from(1),
        ));
        assert!(matches!(
            rejected,
            Classified::Rejected {
                reason: Rejection::Shard {
                    found: 18,
                    expected: 56
                },
                evict: None
            }
        ));

        let forged = format!("{PREFIX}s56.ana.{HOLDER}.room.r0");
        let rejected = classifier.classify(&record(
            &forged,
            None,
            &value("room:r0", "ana", "Fq1", None)?,
            Revision::from(2),
        ));
        assert!(matches!(
            rejected,
            Classified::Rejected {
                reason: Rejection::Key(KvKeyError::ShardMismatch { .. }),
                evict: None
            }
        ));
        Ok(())
    }

    #[test]
    fn topic_classifier_rejects_other_topics_in_its_shard() -> TestResult {
        let classifier = classifier()?;
        let subject = format!("{PREFIX}s56.ana.{HOLDER}.room.r37");
        for payload in [
            value("room:r37", "ana", "Fq1", None)?,
            value("room:r37", "bob", "Fq1", None)?,
        ] {
            let rejected = classifier.classify(&record(&subject, None, &payload, Revision::from(1)));
            assert!(matches!(
                rejected,
                Classified::Rejected {
                    reason: Rejection::Topic,
                    evict: None
                }
            ));
        }
        Ok(())
    }

    fn meta(json: serde_json::Value) -> Result<Meta, Box<dyn std::error::Error>> {
        Ok(Meta::try_from(
            json.as_object().cloned().ok_or("meta is not an object")?,
        )?)
    }

    fn enriched(core: &mut WatchCore, extra: &Meta) -> Result<Diff, Box<dyn std::error::Error>> {
        let batch = core.render();
        let fetched = batch
            .requests()
            .into_iter()
            .map(|entry| {
                let mut merged = entry.meta().as_map().clone();
                merged.extend(extra.as_map().clone());
                Ok(entry.with_meta(Meta::try_from(merged)?))
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        core.commit(batch, fetched).map_err(|(_, err)| err.into())
    }

    #[test]
    fn rendered_view_ref_is_stable_on_identity_and_derived_on_changed_meta() -> TestResult {
        let mut core = WatchCore::default();
        core.apply(put("ana", HOLDER, "A", 1)?);
        let identity = core.flush();
        assert_eq!(refs(identity.joins(), "ana")?, ["A"]);

        core.apply(put("ana", HOLDER, "A", 2)?);
        assert!(core.flush().is_empty());

        let extra = meta(serde_json::json!({"role": "host"}))?;
        core.apply(put("ana", HOLDER, "B", 3)?);
        let diff = enriched(&mut core, &extra)?;
        let stored = StoredRef::parse("B")?;
        let rendered = diff.joins().get(&"ana".parse()?).ok_or("no join")?[0].clone();
        assert_eq!(rendered.phx_ref(), &ViewRef::derive(&stored, rendered.meta()));
        assert_eq!(rendered.phx_ref_prev().map(ToString::to_string), Some("A".to_owned()));
        assert_eq!(rendered.meta(), &extra);
        assert_eq!(refs(diff.leaves(), "ana")?, ["A"]);
        Ok(())
    }

    #[test]
    fn failed_fetch_keeps_the_previous_view_and_retries() -> TestResult {
        let mut core = WatchCore::default();
        core.apply(put("ana", HOLDER, "A", 1)?);
        core.flush();
        core.apply(put("ana", HOLDER, "B", 2)?);
        let batch = core.render();
        let Err((batch, err)) = core.commit(batch, Vec::new()) else {
            return Err("commit accepted a fetch that dropped entries".into());
        };
        assert_eq!(err, FetchError::IdentityChanged);
        core.restore(batch);
        assert_eq!(refs(&core.presences(), "ana")?, ["A"]);
        assert!(core.has_pending());
        assert_eq!(refs(core.flush().joins(), "ana")?, ["B"]);
        Ok(())
    }

    #[test]
    fn renders_metas_in_birth_order() -> TestResult {
        let other = "AAAAAAAAAAAAAAAAAAAAAA";
        let mut core = WatchCore::default();
        core.apply(Observation::Put {
            slot: Slot::new("ana".parse()?, HOLDER.parse()?),
            revision: Revision::from(9),
            entry: StoredEntry::new(StoredRef::parse("Z")?, Meta::default(), Revision::from(9)),
            clock: EntryClock::new(at(9), EffectiveTtl::Unstated),
        });
        core.apply(Observation::Put {
            slot: Slot::new("ana".parse()?, other.parse()?),
            revision: Revision::from(10),
            entry: StoredEntry::new(StoredRef::parse("Y")?, Meta::default(), Revision::from(2)),
            clock: EntryClock::new(at(10), EffectiveTtl::Unstated),
        });
        core.flush();
        assert_eq!(refs(&core.presences(), "ana")?, ["Y", "Z"]);
        let metas: Vec<String> = core
            .metas_for(&"ana".parse()?)
            .iter()
            .map(|entry| entry.phx_ref().to_string())
            .collect();
        assert_eq!(metas, ["Y", "Z"]);
        Ok(())
    }

    #[test]
    fn birth_defaults_to_the_first_revision_when_unrecorded() -> TestResult {
        let payload = value("room:lobby", "ana", "Fq1", None)?;
        let stored = StoredValue::from_json_bytes(&payload)?;
        assert_eq!(StoredEntry::of(&stored, Revision::from(7)).birth(), Revision::from(7));
        Ok(())
    }
}
