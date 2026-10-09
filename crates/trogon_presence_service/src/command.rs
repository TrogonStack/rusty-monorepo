use serde::{Deserialize, Serialize};
use trogon_presence::{
    BeatEntry, EntropyError, EntryTarget, HolderId, LifetimeId, Meta, MutationSequence, OperationId, OwnerEpoch,
    PresenceKey, ReleaseTarget, RequestWindow, RetryWindow, StoredRef, StreamGeneration, Topic, UnixMillis,
    WriteRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestIdentity {
    op: OperationId,
    issued_at: UnixMillis,
    deadline: UnixMillis,
}

impl RequestIdentity {
    pub fn mint(window: RetryWindow) -> Result<Self, EntropyError> {
        let opened = window.open(UnixMillis::now());
        Ok(Self {
            op: OperationId::generate()?,
            issued_at: opened.issued_at(),
            deadline: opened.deadline(),
        })
    }

    pub fn request(self, holder: HolderId) -> WriteRequest {
        WriteRequest::new(self.op, RequestWindow::new(self.issued_at, self.deadline), holder)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRef {
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
}

impl EntryRef {
    pub fn new(lifetime: LifetimeId, mutation_seq: MutationSequence) -> Self {
        Self { lifetime, mutation_seq }
    }

    pub fn target(self) -> EntryTarget {
        EntryTarget::new(self.lifetime, self.mutation_seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeatWire {
    holder: HolderId,
    topic: Topic,
    lifetime: LifetimeId,
    mutation_seq: MutationSequence,
}

impl BeatWire {
    pub fn new(holder: HolderId, topic: Topic, entry: EntryRef) -> Self {
        Self {
            holder,
            topic,
            lifetime: entry.lifetime,
            mutation_seq: entry.mutation_seq,
        }
    }

    pub fn entry(&self) -> BeatEntry {
        BeatEntry::new(self.holder, self.topic.clone(), self.lifetime, self.mutation_seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseWire {
    topic: Topic,
    lifetime: LifetimeId,
}

impl ReleaseWire {
    pub fn target(&self) -> ReleaseTarget {
        ReleaseTarget::new(self.topic.clone(), self.lifetime)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Track {
        topic: Topic,
        request: RequestIdentity,
        holder: HolderId,
        meta: Meta,
        enriched: Option<Meta>,
    },
    Update {
        topic: Topic,
        request: RequestIdentity,
        holder: HolderId,
        meta: Meta,
        enriched: Option<Meta>,
        target: EntryRef,
        expected_ref: Option<StoredRef>,
    },
    Untrack {
        topic: Topic,
        request: RequestIdentity,
        holder: HolderId,
        target: EntryRef,
    },
    Heartbeat {
        entries: Vec<BeatWire>,
    },
    Release {
        request: RequestIdentity,
        holder: HolderId,
        targets: Vec<ReleaseWire>,
    },
}

impl Command {
    pub fn topic(&self) -> Option<&Topic> {
        match self {
            Self::Track { topic, .. } | Self::Update { topic, .. } | Self::Untrack { topic, .. } => Some(topic),
            Self::Heartbeat { .. } | Self::Release { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forwarded {
    key: PresenceKey,
    epoch: OwnerEpoch,
    generation: StreamGeneration,
    reply: Option<String>,
    command: Command,
}

impl Forwarded {
    pub fn new(
        key: PresenceKey,
        epoch: OwnerEpoch,
        generation: StreamGeneration,
        reply: Option<async_nats::Subject>,
        command: Command,
    ) -> Self {
        Self {
            key,
            epoch,
            generation,
            reply: reply.map(|subject| subject.to_string()),
            command,
        }
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn epoch(&self) -> OwnerEpoch {
        self.epoch
    }

    pub fn generation(&self) -> StreamGeneration {
        self.generation
    }

    pub fn reply(&self) -> Option<async_nats::Subject> {
        self.reply.clone().map(async_nats::Subject::from)
    }

    pub fn into_command(self) -> Command {
        self.command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trogon_presence::{EntryRevision, OwnerId};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn forwarded_commands_round_trip() -> TestResult {
        let key: PresenceKey = "ana".parse()?;
        let holder = HolderId::generate()?;
        let request = RequestIdentity::mint(RetryWindow::for_ttl(Default::default()))?;
        let envelope = Forwarded::new(
            key.clone(),
            OwnerEpoch::new(EntryRevision::from(7), OwnerId::generate()?),
            StreamGeneration::generate()?,
            Some(async_nats::Subject::from("_INBOX_U.ana.c1.n1")),
            Command::Track {
                topic: "room:lobby".parse()?,
                request,
                holder,
                meta: Meta::default(),
                enriched: None,
            },
        );
        let encoded = serde_json::to_value(&envelope)?;
        assert_eq!(encoded["command"]["op"], "track");
        let decoded: Forwarded = serde_json::from_value(encoded)?;
        assert_eq!(decoded, envelope);
        assert_eq!(
            decoded.reply().map(|subject| subject.to_string()).as_deref(),
            Some("_INBOX_U.ana.c1.n1")
        );
        Ok(())
    }
}
