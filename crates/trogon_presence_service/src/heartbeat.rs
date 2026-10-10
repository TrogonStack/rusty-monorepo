use std::time::Duration;

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use trogon_presence::{BeatStatus, HolderId, LifetimeId, MutationSequence, PresenceKey, Topic};

use crate::admission::Counter;
use crate::command::{BeatWire, Command, EntryRef};
use crate::reply::{ErrorReply, ReplyCode, Response, HEADER_CODE};
use crate::shard_owner::parse_required;
use crate::writes::Writes;

pub const HEARTBEAT_MANY_MAX_ENTRIES: usize = 256;
const HEARTBEAT_MANY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManyEntry {
    key: PresenceKey,
    holder: HolderId,
    topic: Topic,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManyBody {
    entries: Vec<ManyEntry>,
}

#[derive(Debug, Deserialize)]
struct OwnerReply {
    entries: Vec<String>,
}

#[derive(Serialize)]
struct ManyReply {
    interval: u64,
    entries: Vec<&'static str>,
}

struct Group {
    key: PresenceKey,
    slots: Vec<usize>,
    entries: Vec<BeatWire>,
}

pub(crate) struct HeartbeatMany<'a> {
    writes: &'a Writes,
}

impl<'a> HeartbeatMany<'a> {
    pub(crate) fn new(writes: &'a Writes) -> Self {
        Self { writes }
    }

    pub(crate) async fn handle(&self, payload: &[u8]) -> Response {
        let body: ManyBody = match parse_required(payload) {
            Ok(body) => body,
            Err(response) => return response,
        };
        if body.entries.len() > HEARTBEAT_MANY_MAX_ENTRIES {
            return ErrorReply::new(ReplyCode::InvalidRequest)
                .detail(format!(
                    "at most {HEARTBEAT_MANY_MAX_ENTRIES} entries per heartbeat-many"
                ))
                .into();
        }
        let total = body.entries.len();
        let mut groups: Vec<Group> = Vec::new();
        for (slot, entry) in body.entries.into_iter().enumerate() {
            let wire = BeatWire::new(
                entry.holder,
                entry.topic,
                EntryRef::new(entry.lifetime, entry.mutation_seq),
            );
            match groups.iter_mut().find(|group| group.key == entry.key) {
                Some(group) => {
                    group.slots.push(slot);
                    group.entries.push(wire);
                }
                None => groups.push(Group {
                    key: entry.key,
                    slots: vec![slot],
                    entries: vec![wire],
                }),
            }
        }
        let mut statuses = vec![BeatStatus::Unavailable.as_str(); total];
        let results = join_all(groups.iter().map(|group| async {
            tokio::time::timeout(HEARTBEAT_MANY_TIMEOUT, self.beat(group))
                .await
                .ok()
                .flatten()
        }))
        .await;
        for (group, result) in groups.iter().zip(results) {
            if let Some(found) = result {
                for (slot, status) in group.slots.iter().zip(found) {
                    if let Some(cell) = statuses.get_mut(*slot) {
                        *cell = status;
                    }
                }
            }
        }
        Response::ok_json(&ManyReply {
            interval: self.writes.heartbeat_secs(),
            entries: statuses,
        })
    }

    async fn beat(&self, group: &Group) -> Option<Vec<&'static str>> {
        let command = Command::Heartbeat {
            entries: group.entries.clone(),
        };
        let (subject, payload) = self.writes.envelope(&group.key, None, command).await.ok()?;
        let message = self.writes.request(subject, payload).await.ok()?;
        self.writes.count(Counter::Forwarded);
        let code_ok = message
            .headers
            .as_ref()
            .and_then(|headers| headers.get(HEADER_CODE))
            .is_none_or(|code| code.as_str() == ReplyCode::Ok.as_str());
        if !code_ok {
            return None;
        }
        let reply: OwnerReply = serde_json::from_slice(&message.payload).ok()?;
        if reply.entries.len() != group.entries.len() {
            return None;
        }
        Some(reply.entries.iter().map(|status| status_of(status)).collect())
    }
}

fn status_of(status: &str) -> &'static str {
    [BeatStatus::Ok, BeatStatus::Gone, BeatStatus::Conflict]
        .into_iter()
        .map(BeatStatus::as_str)
        .find(|known| *known == status)
        .unwrap_or(BeatStatus::Unavailable.as_str())
}
