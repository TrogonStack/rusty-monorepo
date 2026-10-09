use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::{BucketName, PresenceConfig};
use crate::constants::{KV_SUBJECT_PREFIX, TOKEN_SEPARATOR};
use crate::holder::HolderId;
use crate::key::PresenceKey;
use crate::kv_key::{EntryKey, KvKey};
use crate::meta::Meta;
use crate::operation::UnixMillis;
use crate::phx_ref::StoredRef;
use crate::position::{LifetimeId, MutationSequence};
use crate::revision::Revision;
use crate::shard::{ShardCount, ViewShard};
use crate::topic::Topic;
use crate::value::StoredValue;
use crate::watch::engine::{is_leave, EffectiveTtl};
use crate::watch::replay::{RawRecord, ReplayFilters};

const ANY_TOKEN: &str = "*";
const ANY_TAIL: &str = ">";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryScope {
    Topic(Topic),
    KeyInTopic(PresenceKey, Topic),
    Key(PresenceKey),
    Holder(HolderId),
}

impl EntryScope {
    pub(crate) fn filters(&self, bucket: &BucketName, shards: ShardCount) -> ReplayFilters {
        let shard = |topic: &Topic| shards.token(ViewShard::of(topic, shards));
        let tokens = match self {
            Self::Topic(topic) => [
                shard(topic),
                ANY_TOKEN.to_owned(),
                ANY_TOKEN.to_owned(),
                topic.tokens().to_owned(),
            ],
            Self::KeyInTopic(key, topic) => [
                shard(topic),
                key.token().to_owned(),
                ANY_TOKEN.to_owned(),
                topic.tokens().to_owned(),
            ],
            Self::Key(key) => [
                ANY_TOKEN.to_owned(),
                key.token().to_owned(),
                ANY_TOKEN.to_owned(),
                ANY_TAIL.to_owned(),
            ],
            Self::Holder(holder) => [
                ANY_TOKEN.to_owned(),
                ANY_TOKEN.to_owned(),
                holder.to_string(),
                ANY_TAIL.to_owned(),
            ],
        };
        let mut filter = subject_prefix(bucket);
        filter.push_str(&tokens.join(&TOKEN_SEPARATOR.to_string()));
        ReplayFilters::from(filter)
    }

    fn admits(&self, entry: &EntryKey) -> bool {
        match self {
            Self::Topic(topic) => entry.topic() == topic,
            Self::KeyInTopic(key, topic) => entry.topic() == topic && entry.key() == key,
            Self::Key(key) => entry.key() == key,
            Self::Holder(holder) => entry.holder() == holder,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredPresence {
    pub topic: Topic,
    pub key: PresenceKey,
    pub holder: HolderId,
    pub phx_ref: StoredRef,
    pub phx_ref_prev: Option<StoredRef>,
    pub meta: Meta,
    pub lifetime: LifetimeId,
    pub mutation_seq: MutationSequence,
    pub revision: Revision,
    pub published: UnixMillis,
    pub expires_at: Option<UnixMillis>,
}

impl StoredPresence {
    pub fn is_live_at(&self, now: UnixMillis) -> bool {
        self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Inventory {
    entries: Vec<StoredPresence>,
    unreadable: usize,
}

impl Inventory {
    pub fn entries(&self) -> &[StoredPresence] {
        &self.entries
    }

    pub fn into_entries(self) -> Vec<StoredPresence> {
        self.entries
    }

    pub fn unreadable(&self) -> usize {
        self.unreadable
    }

    pub fn live_at(&self, now: UnixMillis) -> impl Iterator<Item = &StoredPresence> {
        self.entries.iter().filter(move |entry| entry.is_live_at(now))
    }

    pub fn count_at(&self, now: UnixMillis) -> PresenceCount {
        let live: Vec<_> = self.live_at(now).collect();
        PresenceCount {
            keys: live.iter().map(|entry| &entry.key).collect::<BTreeSet<_>>().len(),
            metas: live.len(),
        }
    }

    pub(crate) fn absorb(&mut self, scope: &EntryScope, config: &PresenceConfig, record: &RawRecord) {
        if is_leave(record.headers()) {
            return;
        }
        match decode(scope, config, record) {
            Decoded::Entry(entry) => self.entries.push(*entry),
            Decoded::OutOfScope => {}
            Decoded::Unreadable => self.unreadable += 1,
        }
    }

    pub(crate) fn sort(&mut self) {
        self.entries.sort_by(|left, right| {
            (&left.topic, &left.key, &left.holder).cmp(&(&right.topic, &right.key, &right.holder))
        });
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PresenceCount {
    pub keys: usize,
    pub metas: usize,
}

enum Decoded {
    Entry(Box<StoredPresence>),
    OutOfScope,
    Unreadable,
}

fn subject_prefix(bucket: &BucketName) -> String {
    format!("{KV_SUBJECT_PREFIX}{TOKEN_SEPARATOR}{bucket}{TOKEN_SEPARATOR}")
}

fn decode(scope: &EntryScope, config: &PresenceConfig, record: &RawRecord) -> Decoded {
    let Some(token) = record.subject().strip_prefix(&subject_prefix(config.bucket())) else {
        return Decoded::Unreadable;
    };
    let Ok(entry) = KvKey::try_from(token.to_owned()).and_then(|kv| EntryKey::decode(&kv, config.shards())) else {
        return Decoded::OutOfScope;
    };
    if !scope.admits(&entry) {
        return Decoded::OutOfScope;
    }
    let Ok(value) = StoredValue::from_json_bytes(record.payload()) else {
        return Decoded::Unreadable;
    };
    if value.topic() != entry.topic() || value.key() != entry.key() {
        return Decoded::Unreadable;
    }
    let published = record.published().get();
    let ttl = match EffectiveTtl::from_headers(record.headers()) {
        EffectiveTtl::Expires(ttl) => Some(ttl),
        EffectiveTtl::Never => None,
        EffectiveTtl::Unstated => Some(config.lease_ttl().get()),
    };
    Decoded::Entry(Box::new(StoredPresence {
        topic: entry.topic().clone(),
        key: entry.key().clone(),
        holder: *entry.holder(),
        phx_ref: value.phx_ref().clone(),
        phx_ref_prev: value.phx_ref_prev().cloned(),
        meta: value.meta().clone(),
        lifetime: value.lifetime(),
        mutation_seq: value.mutation_seq(),
        revision: record.stream_seq(),
        published: UnixMillis::from(published),
        expires_at: ttl.and_then(|ttl| published.checked_add(ttl)).map(UnixMillis::from),
    }))
}

impl From<SystemTime> for UnixMillis {
    fn from(time: SystemTime) -> Self {
        let elapsed = time.duration_since(UNIX_EPOCH).unwrap_or_default();
        Self::from(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn scopes_filter_the_entry_layout() -> TestResult {
        let bucket: BucketName = "PRESENCE_V1".parse()?;
        let shards = ShardCount::DEFAULT;
        let topic: Topic = "room:lobby".parse()?;
        let key: PresenceKey = "alice".parse()?;
        let holder: HolderId = "q3V9hX0bS2mWf1ZkR8aT1A".parse()?;
        let shard = shards.token(ViewShard::of(&topic, shards));
        let filter = |scope: EntryScope| scope.filters(&bucket, shards).as_slice().to_vec();
        assert_eq!(
            filter(EntryScope::Topic(topic.clone())),
            [format!("$KV.PRESENCE_V1.{shard}.*.*.{}", topic.tokens())]
        );
        assert_eq!(
            filter(EntryScope::KeyInTopic(key.clone(), topic.clone())),
            [format!("$KV.PRESENCE_V1.{shard}.{}.*.{}", key.token(), topic.tokens())]
        );
        assert_eq!(
            filter(EntryScope::Key(key.clone())),
            [format!("$KV.PRESENCE_V1.*.{}.*.>", key.token())]
        );
        assert_eq!(
            filter(EntryScope::Holder(holder)),
            [format!("$KV.PRESENCE_V1.*.*.{holder}.>")]
        );
        Ok(())
    }
}
